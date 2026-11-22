//! Unit `sys-tutorial`. The first ten minutes: the tutorial script model as data the client drives.
//! DESIGN.md L252-275, L47 (time to first fight: 10-minute median).
//!
//! Steps: (1) open the tutorial variant of the 22 m fast attack craft: afloat, under-beamed, unarmed, no
//! radar (the fully fitted version is the reward for finishing); (2) drag the beam handle until Float shows
//! Pass; (3) drop four kits from the tray (gun, radar mast, engine, fuel tank) and watch the conduits
//! auto-route; (4) apply the Ship's Notes Fix for the blocked gun arc; (5) splash and steer with WASD while
//! the guns track and hit drones on their own; (6) beat the patrol boat, move the Aggression slider, splash
//! again. Gates: time to first fight (open tutorial to the first splash where the player's guns fire at
//! drones) median at most 10 minutes; time to first splash kill median under 15 minutes and 80% within 20.
//! Fallback levels, applied in order with a re-test after each: (1) the hull starts with Float at Pass and
//! step 2 becomes a 20 s demo; (2) engine and fuel tank pre-fitted, the player drops two kits; (3) the
//! blocked-arc Fix is applied automatically with its ghost preview played as a demo; (4) the tutorial is one
//! drag: the fully fitted variant minus its gun.
//!
//! The script is data (`crates/sim-systems/data/tutorial.ron`, one script per fallback level, all text as
//! translation keys in `game/i18n/tutorial.csv`). The client owns presentation and reports what happened as
//! [`Signal`]s; [`TutorialRun::signal`] advances the state machine and returns [`Effect`]s to carry out.
//! Nothing here touches the simulation: gating is by the client disabling controls named in a step's
//! `allow` list. The run writes step timestamps to a local log; nothing is sent automatically (L266).
use crate::prelude::*;
use crate::protocol::event::SimEvent;
use content::settings::SplashOptions;

pub const TUTORIAL_DATA: &str = include_str!("../../data/tutorial.ron");

/// Something the client or the simulation reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Signal {
    /// The player dismissed a text-only step.
    Continue,
    /// A gauge changed check state (`float`, `speed`, `power`...).
    Gauge { gauge: String, pass: bool },
    HandleChanged { handle: String },
    KitPlaced { kit: KitId },
    PartPlaced { part: PartId },
    NoteFixApplied { note: String },
    NotesOpen { count: u16 },
    /// The ids (`note_id`) of every note open on the current design, after each notes recompute.
    OpenNotes { ids: Vec<String> },
    SliderMoved { key: String },
    SplashStarted,
    SplashEnded,
    /// From the simulation: the player's guns fired, an enemy was destroyed, the player steered.
    PlayerGunsFired,
    EnemyDestroyed { class: Option<ClassId> },
    HelmInput,
    Designated,
    /// Seconds elapsed since the step began (the client ticks this once a second for timed demos).
    Elapsed { seconds: u32 },
}

/// What completes a step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// `After` is a timed escape (a step that ends through one is logged as timed out); `Dwell` is a minimum time
/// the step lasts once the player has done the thing (the drone basin stays open long enough to watch the
/// guns work), and is never counted as an escape.
pub enum Trigger { Continue, GaugePass(String), KitsPlaced(Vec<KitId>), NoteFixed(String), SplashStarted, PlayerGunsFired, EnemyDestroyed, SliderMoved(String), HelmInput, After { seconds: u32 }, Dwell { seconds: u32 }, All(Vec<Trigger>), Any(Vec<Trigger>) }

/// What the client does when a step begins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// Load a stock blueprint into the editor (path under `blueprints/`).
    LoadBlueprint(String),
    /// Show the step: text key, and the UI element to point at (stable control id, for example
    /// `editor.handle.beam`, `editor.tray.kit.gun`, `editor.notes.fix`, `editor.launch`, `hud.slider.aggression`).
    Show { text: TextKey, point_at: Option<String> },
    /// Controls enabled during the step; everything else in the editor is inert. Empty = everything.
    Allow(Vec<String>),
    /// Play a scripted demo instead of asking the player (fallback levels): a named client animation.
    Demo(String),
    /// Place kits for the player (fallback level 2).
    PlaceKits(Vec<(KitId, content::cell::CellPos)>),
    ApplyNoteFix(String),
    /// Set the splash basin options for the next splash.
    SplashOptions(SplashOptions),
    /// Unlock the reward blueprint and finish.
    Reward(String),
    Finished,
    /// Timestamp row for the local log.
    Log { step: String, seconds: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub text: TextKey,
    #[serde(default)]
    pub point_at: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    /// Effects to run on entering the step, before showing it.
    #[serde(default)]
    pub on_enter: Vec<Effect>,
    pub complete: Trigger,
    /// Effects to run when the step ends through a timed escape: its trigger was met only because an
    /// `After` leaf ran out (for example, load the fitted hull when the kit step times out with kits
    /// missing, so the next steps can still be played). Run before the step's log row.
    #[serde(default)]
    pub on_escape: Vec<Effect>,
    /// Marks the two gate moments (L261-264).
    #[serde(default)]
    pub marks_first_fight: bool,
    #[serde(default)]
    pub marks_first_kill: bool,
    /// Which of the spec's numbered steps (L254-259) this script step belongs to, counted from 1, so the
    /// player reads "Step 3 of 6" however many script steps one lesson takes; 0 = unnumbered (the closing step).
    #[serde(default)]
    pub lesson: u8,
    /// What the player achieved, shown once the step is over (a fact that holds whenever the step's condition
    /// does, so it also reads true when the step completed on entry).
    #[serde(default)]
    pub done: Option<TextKey>,
}

/// Where a lesson kit settles in the tutorial dock (DESIGN.md L257: the player drops four kits from the tray).
/// The cell is a ghost anchor (`BpEditor.ghost_move`): `x` and `y` in hull cells, `dz` across from the hull's
/// centre cell (the tutorial hulls share length and depth but not beam). The dock places a dropped kit at the
/// valid pose nearest the spot, wherever on the hull it was dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitSpot {
    pub kit: KitId,
    pub x: i16,
    pub y: i16,
    #[serde(default)]
    pub dz: i16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub id: String,
    /// 0 = the full tutorial; 1..=4 = the fallback levels in order.
    pub fallback_level: u8,
    pub blueprint: String,
    pub reward: String,
    pub steps: Vec<Step>,
    /// Marked spots for the lesson kits (see [`KitSpot`]); a kit whose family has none follows the cursor.
    #[serde(default)]
    pub spots: Vec<KitSpot>,
}

/// The timing log written to the user data folder (L266).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TutorialLog {
    pub script: String,
    pub fallback_level: u8,
    pub steps: Vec<(String, f64)>,
    pub first_fight_s: Option<f64>,
    pub first_kill_s: Option<f64>,
    pub finished_s: Option<f64>,
    /// Ids of the steps that ended through their timed escape rather than the player's action, in order
    /// (where playtesters get stuck).
    #[serde(default)]
    pub timed_out: Vec<String>,
}

/// A tutorial in progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TutorialRun {
    pub script: Script,
    pub step: usize,
    pub elapsed_s: f64,
    pub step_started_s: f64,
    /// One flag per leaf of the current step's trigger, depth-first (a `KitsPlaced` gives one per kit).
    pub progress: Vec<bool>,
    pub log: TutorialLog,
    pub finished: bool,
    /// Gauge states, kits and Fixes reported so far (the client reports them only on change).
    #[serde(default)]
    pub(crate) mem: Remembered,
    /// The largest `Signal::Elapsed` value seen in the current step.
    #[serde(default)]
    pub(crate) step_signal_s: u32,
}

/// Parse every script in the embedded data. Unit: `sys-tutorial`.
///
/// Scripts come back in file order. The data is embedded and checked by tests, so a parse failure cannot
/// ship; should one happen anyway the list is empty and the client reports the tutorial as missing.
pub fn scripts() -> Vec<Script> {
    content::data::parse_ron::<Vec<Script>>(TUTORIAL_DATA, TUTORIAL_FILE).unwrap_or_default()
}
/// The script for a fallback level (0 by default; the level in force is a committed tuning choice).
/// Unit: `sys-tutorial`.
pub fn script_for(level: u8) -> Option<Script> { scripts().into_iter().find(|s| s.fallback_level == level) }

impl TutorialRun {
    /// Begin a script; returns the effects of entering its first step. Unit: `sys-tutorial`.
    pub fn start(script: Script) -> (TutorialRun, Vec<Effect>) {
        let log = TutorialLog { script: script.id.clone(), fallback_level: script.fallback_level, ..TutorialLog::default() };
        let mut run = TutorialRun { script, step: 0, elapsed_s: 0.0, step_started_s: 0.0, progress: Vec::new(), log, finished: false, mem: Remembered::default(), step_signal_s: 0 };
        let mut out = Vec::new();
        run.enter(&mut out);
        (run, out)
    }
    /// Advance wall time (the client passes its own frame time; pauses over 2 minutes are excluded by the
    /// client, L79). Non-finite and negative frame times are ignored.
    pub fn tick(&mut self, dt_s: f64) { if dt_s.is_finite() && dt_s > 0.0 { self.elapsed_s += dt_s; } }
    /// Feed one signal; returns the effects to carry out (none when the step is not complete yet, apart from
    /// a `Log` row when the signal is a gate moment). Unit: `sys-tutorial`.
    pub fn signal(&mut self, signal: &Signal) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.finished { return out; }
        if self.step >= self.script.steps.len() {
            self.finish(&mut out);
            return out;
        }
        self.observe(signal, &mut out);
        self.latch(Some(signal));
        if self.step_met() {
            self.leave(&mut out);
            self.enter(&mut out);
        }
        out
    }
    pub fn current(&self) -> Option<&Step> { self.script.steps.get(self.step) }
}

/// Map a simulation event from the tutorial's splash to a signal, if it is one the script can wait on.
/// Unit: `sys-tutorial`.
///
/// - A mount on the player's hull fired: `PlayerGunsFired`.
/// - Any other vessel was lost (the basin holds only the player and the targets; the killer's id may be
///   scrubbed, so it is not required): `EnemyDestroyed`. So does another vessel put out of the fight
///   (`Disabled`: colours struck or no working propulsion), which the splash report already counts as
///   destroyed. A patrol boat that strikes stays afloat, so waiting for its loss held the patrol step to its
///   escape. A body the player shot down is not a kill: the basin's drones and patrol boat are hulls, and the
///   bodies the player's mounts shoot down are missiles (the patrol boat's, or the player's own in the first
///   second of a drone basin), which ended the drone and patrol steps without a target touched.
/// - The player took the helm: `HelmInput`. The client also sends `HelmInput` itself on steering input.
///
/// With `player_vessel` unknown (`EntityId::NONE`) nothing maps, because the player's own loss could not be
/// told apart from a kill.
pub fn signal_from_event(event: &SimEvent, player_vessel: EntityId) -> Option<Signal> {
    use crate::protocol::command::ControlRole;
    use sim_core::PhysEvent;
    if player_vessel.is_none() { return None; }
    match event {
        SimEvent::Phys(PhysEvent::MountFired { vessel, rounds, .. }) if *vessel == player_vessel && *rounds > 0 => Some(Signal::PlayerGunsFired),
        SimEvent::Phys(PhysEvent::VesselLost { vessel, .. }) if *vessel != player_vessel && !vessel.is_none() => Some(Signal::EnemyDestroyed { class: None }),
        SimEvent::Phys(PhysEvent::StatusChanged { vessel, status: sim_core::vessel::VesselStatus::Disabled }) if *vessel != player_vessel && !vessel.is_none() => Some(Signal::EnemyDestroyed { class: None }),
        SimEvent::ControlChanged { vessel, role: ControlRole::Helm, holder: ControlHolder::Player } if *vessel == player_vessel => Some(Signal::HelmInput),
        _ => None,
    }
}

/// Stable id of a Ship's Note kind, the `note` the client reports in `Signal::NoteFixApplied` and scripts
/// name in `Trigger::NoteFixed` (snake case of the kind; `arc_blocked` for a blocked weapon arc).
pub fn note_id(kind: &crate::editor::notes::NoteKind) -> &'static str {
    use crate::editor::notes::NoteKind as K;
    match kind {
        K::FloatFail => "float_fail",
        K::NoPropulsion => "no_propulsion",
        K::NoGeneration => "no_generation",
        K::NoSteering => "no_steering",
        K::TagFails(_) => "tag_fails",
        K::RollsOver { .. } => "rolls_over",
        K::ArcBlocked { .. } => "arc_blocked",
        K::Unpowered => "unpowered",
        K::Unconnected => "unconnected",
        K::NoMagazine => "no_magazine",
        K::NoDirector => "no_director",
        K::PinnedNodeLostInput(_) => "pinned_node_lost_input",
        K::PinnedRouteBroken(_) => "pinned_route_broken",
        K::GaugeCaution(_) => "gauge_caution",
        K::AutoSetDropped(_) => "auto_set_dropped",
        K::PartRemovedByMigration(_) => "part_removed_by_migration",
        K::OverTonnage => "over_tonnage",
        K::OverComplexity => "over_complexity",
        K::Advisory(_) => "advisory",
    }
}

/// Plain-text form of the log for Settings > Playtest > Copy timing log (L80). Unit: `sys-tutorial`.
///
/// One fact per line, times from opening the tutorial, for pasting into the playtest issue form. Step ids
/// and the script id are written through a filter that keeps the text on one line each.
pub fn log_text(log: &TutorialLog) -> String {
    use core::fmt::Write;
    fn one_line(s: &str) -> String { s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(80).collect() }
    let mut t = String::new();
    let _ = writeln!(t, "Black Pennant tutorial timing log");
    let _ = writeln!(t, "Script: {} (fallback level {})", one_line(&log.script), log.fallback_level);
    let _ = writeln!(t, "Time to first fight: {}", log.first_fight_s.map_or_else(|| "not reached".to_string(), fmt_s));
    let _ = writeln!(t, "Time to first splash kill: {}", log.first_kill_s.map_or_else(|| "not reached".to_string(), fmt_s));
    let _ = writeln!(t, "Finished: {}", log.finished_s.map_or_else(|| "no".to_string(), fmt_s));
    let timed_out: Vec<String> = log.timed_out.iter().map(|id| one_line(id)).collect();
    let _ = writeln!(t, "Timed out: {}", if timed_out.is_empty() { "none".to_string() } else { timed_out.join(", ") });
    let _ = writeln!(t, "Steps (ended at, took):");
    let mut prev = 0.0;
    for (id, at) in &log.steps {
        let at = clean(*at);
        let _ = writeln!(t, "{}: {}, {}", one_line(id), fmt_s(at), fmt_s((at - prev).max(0.0)));
        prev = at;
    }
    t
}

// ---------------------------------------------------------------------------------------------------------
// Implementation (unit `sys-tutorial`).
//
// Semantics, in one place:
// - A step's trigger is a tree. Its leaves, in depth-first order, are the run's `progress` flags; a
//   `KitsPlaced(kits)` contributes one leaf per kit, every other non-compound trigger one leaf. Leaves latch:
//   once met they stay met for the step. `All([])` and `Any([])` (and `KitsPlaced([])`) count as met, so an
//   empty condition can never hold a player in place.
// - Edge-triggered state the client reports only on change (gauge checks, placed kits, applied Fixes, "no
//   notes open") is remembered across steps. A step whose condition is already true when it begins (the
//   Float gauge already at Pass, a kit dropped early) completes at once.
// - `After { seconds }` and `Dwell { seconds }` are met when the step's age reaches `seconds`; only `After` is a
//   timed escape (see "times out" below). The age is the run's own clock
//   (`elapsed_s - step_started_s`, advanced by `tick`); a client that never ticks can drive it with
//   `Signal::Elapsed` (seconds since the step began) instead. Timed leaves are re-checked on every signal,
//   so the client sends `Elapsed` about once a second.
// - `NoteFixed(n)` is met by `NoteFixApplied { note: n }`, or by `NotesOpen { count: 0 }` or an `OpenNotes` list
//   without `n` arriving during the step (nothing is left to fix, or the note never came up on this design; the
//   client sends `OpenNotes` after every notes recompute of the current design, so a step that would point at a
//   note that is not there ends on the first recompute). When a step completes while a `NoteFixed(n)` leaf is still unmet (it completed through an
//   `Any` escape), the run emits `ApplyNoteFix(n)`: the tutorial applies the Fix for the player. Fallback 3
//   and the escape of step 4 both use this. The client treats a Fix for a note that is not open as a no-op.
// - The gate stamps (`first_fight_s`, `first_kill_s`) are taken from the first `PlayerGunsFired` and the
//   first `EnemyDestroyed` the run sees, whichever step is showing, so the time is the event's time and not
//   the time the step that waits for it ends. A step that completes through a timed escape never stamps.
//   `marks_first_fight` / `marks_first_kill` say which step the gate belongs to (shipped data has exactly
//   one of each, on the steps that wait for those signals); when a marked step ends, a `Log` row names the
//   gate so the client's file shows it even if the gate was missed.
// - A step "times out" when it completes but would not have with its `After` leaves unmet. Its `on_escape`
//   effects run (the shipped kit steps load the fully fitted hull, so nobody reaches the drones without a
//   gun), its id goes to `TutorialLog::timed_out`, and a `Log` row `<id>.timed_out` is emitted, so the
//   playtest log shows where players stall.
// - Control gating: see [`Step::allows`]. Text-only steps list `tutorial.continue` so the editor is inert
//   while the player reads; from the first splash on, steps allow everything, so a player can always return
//   to the design and repair it.
// - `Demo(name)`: the client plays the animation and, when it ends, reports `Signal::Continue`
//   (`BpTutorial::demo_finished`). Demo steps also carry a timed escape.
// - Finishing emits `Reward(script.reward)` (when the script names one) and then `Finished`. Scripts do not
//   put `Reward` or `Finished` in `on_enter`.
// ---------------------------------------------------------------------------------------------------------

/// Path of the embedded scripts, for parse error messages.
const TUTORIAL_FILE: &str = "crates/sim-systems/data/tutorial.ron";

/// Control id prefixes that are never disabled: the tutorial panel itself (continue, leave), the cameras and
/// the pause menu. The client checks controls with [`Step::allows`] or [`TutorialRun::allows`].
pub const ALWAYS_ALLOWED: &[&str] = &["tutorial", "editor.camera", "sortie.camera", "menu"];

/// Upper bound on steps auto-completed in one call, so a malformed script cannot loop.
const MAX_CHAIN: usize = 256;

/// Whether control id `control` sits under `prefix` (equal, or `prefix` followed by a dot).
fn under(control: &str, prefix: &str) -> bool {
    !prefix.is_empty() && control.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// A placed kit satisfies a wanted kit when the ids are equal or the placed kit is a variant in the wanted
/// kit's family (`kit.gun` accepts `kit.gun.20mm`).
pub fn kit_matches(wanted: &KitId, placed: &KitId) -> bool { under(placed.as_str(), wanted.as_str()) }

impl Script {
    /// The highest lesson number, for "Step n of N".
    pub fn lessons(&self) -> u8 { self.steps.iter().map(|s| s.lesson).max().unwrap_or(0) }
    /// The marked spot for a kit, by family.
    pub fn spot_for(&self, kit: &KitId) -> Option<&KitSpot> { self.spots.iter().find(|s| kit_matches(&s.kit, kit)) }
}

impl TutorialRun {
    /// Kits the current step still waits for and those already counted, in the step's order (its `KitsPlaced`
    /// leaves); both empty for a step that waits on no kit.
    pub fn kits_progress(&self) -> (Vec<KitId>, Vec<KitId>) {
        let (mut want, mut have) = (Vec::new(), Vec::new());
        let Some(step) = self.current() else { return (want, have) };
        let mut base = 0;
        step.complete.for_each_leaf(&mut base, &mut |i, leaf| {
            if let Leaf::Kit(k) = leaf {
                if self.progress.get(i).copied().unwrap_or(false) { have.push(k.clone()) } else { want.push(k.clone()) }
            }
        });
        (want, have)
    }
    /// The `done` line of the step before the current one when it ended by the player's doing (not through its
    /// timed escape), for the card to confirm what just happened.
    pub fn previous_done(&self) -> Option<&TextKey> {
        let prev = self.script.steps.get(self.step.checked_sub(1)?)?;
        if self.log.timed_out.iter().any(|id| *id == prev.id) { return None; }
        prev.done.as_ref()
    }
    /// Whether the current step waits on kits being placed (the dock then settles lesson kits on their spots).
    pub fn wants_kits(&self) -> bool {
        self.current().is_some_and(|s| { let mut hit = false; let mut b = 0; s.complete.for_each_leaf(&mut b, &mut |_, l| hit |= matches!(l, Leaf::Kit(_))); hit })
    }
}

impl Step {
    /// Whether a control (stable dotted id such as `editor.tray.kit.gun`) is enabled during this step. An empty
    /// `allow` list enables everything; otherwise a control is enabled when it equals an entry or sits under
    /// one (`editor.tray` enables `editor.tray.kit.gun`), or sits under [`ALWAYS_ALLOWED`].
    pub fn allows(&self, control: &str) -> bool {
        self.allow.is_empty()
            || ALWAYS_ALLOWED.iter().any(|p| under(control, p))
            || self.allow.iter().any(|p| under(control, p))
    }
}

impl Trigger {
    /// Number of progress leaves (see the module notes).
    fn leaf_count(&self) -> usize {
        match self {
            Trigger::KitsPlaced(kits) => kits.len(),
            Trigger::All(ts) | Trigger::Any(ts) => ts.iter().map(Trigger::leaf_count).sum(),
            _ => 1,
        }
    }

    /// Evaluate against latched leaves, consuming `leaves` in depth-first order.
    fn met(&self, leaves: &mut core::slice::Iter<'_, bool>) -> bool {
        match self {
            Trigger::KitsPlaced(kits) => kits.iter().fold(true, |ok, _| leaves.next().copied().unwrap_or(false) && ok),
            // Every child is evaluated (no short-circuit) so the leaf cursor stays aligned.
            Trigger::All(ts) => ts.iter().fold(true, |ok, t| t.met(leaves) && ok),
            Trigger::Any(ts) => ts.is_empty() || ts.iter().fold(false, |ok, t| t.met(leaves) || ok),
            _ => leaves.next().copied().unwrap_or(false),
        }
    }

    /// Whether the trigger is met with every `After` leaf counted as unmet: false means the step is ending
    /// through its timed escape.
    fn met_without_timers(&self, progress: &[bool]) -> bool {
        let mut masked = progress.to_vec();
        let mut base = 0;
        self.for_each_leaf(&mut base, &mut |i, leaf| {
            if let (Leaf::Other(Trigger::After { .. }), Some(slot)) = (leaf, masked.get_mut(i)) { *slot = false; }
        });
        self.met(&mut masked.iter())
    }

    /// `NoteFixed` leaves with their leaf index, depth-first.
    fn note_leaves(&self, base: &mut usize, out: &mut Vec<(usize, String)>) {
        match self {
            Trigger::KitsPlaced(kits) => *base += kits.len(),
            Trigger::All(ts) | Trigger::Any(ts) => ts.iter().for_each(|t| t.note_leaves(base, out)),
            Trigger::NoteFixed(n) => { out.push((*base, n.clone())); *base += 1; }
            _ => *base += 1,
        }
    }

    /// Call `f(leaf_index, leaf)` for every leaf; a kit leaf is passed as a one-kit `KitsPlaced`.
    fn for_each_leaf(&self, base: &mut usize, f: &mut dyn FnMut(usize, Leaf<'_>)) {
        match self {
            Trigger::KitsPlaced(kits) => for k in kits { f(*base, Leaf::Kit(k)); *base += 1; },
            Trigger::All(ts) | Trigger::Any(ts) => ts.iter().for_each(|t| t.for_each_leaf(base, f)),
            other => { f(*base, Leaf::Other(other)); *base += 1; }
        }
    }
}

/// One progress leaf, borrowed from the trigger tree.
#[derive(Clone, Copy)]
enum Leaf<'a> { Kit(&'a KitId), Other(&'a Trigger) }

/// What a run remembers across steps because the client reports it only on change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Remembered {
    /// Latest check state per gauge.
    gauges: BTreeMap<String, bool>,
    /// Every kit placed so far, in order (a multiset: two guns are two entries).
    kits: Vec<KitId>,
    /// Notes whose Fix has been applied.
    fixed: BTreeSet<String>,
}

/// Seconds as a finite, non-negative value.
fn clean(s: f64) -> f64 { if s.is_finite() && s > 0.0 { s } else { 0.0 } }

impl TutorialRun {
    fn now(&self) -> f64 { clean(self.elapsed_s) }

    /// Seconds the current step has been showing: the run's own clock once `tick` has advanced it (a stale
    /// `Elapsed` from the previous step cannot fire the next step's escape), else the last `Elapsed` value.
    fn step_age(&self) -> f64 {
        if self.now() > 0.0 { (self.now() - clean(self.step_started_s)).max(0.0) } else { self.step_signal_s as f64 }
    }

    /// Whether a control is enabled now: the current step's rule, or everything when no step is showing.
    pub fn allows(&self, control: &str) -> bool {
        match self.current() { Some(step) if !self.finished => step.allows(control), _ => true }
    }

    /// Make `progress` match the current step's leaf count (it is public, so it may have been resized).
    fn fit_progress(&mut self) {
        let n = self.current().map_or(0, |s| s.complete.leaf_count());
        if self.progress.len() != n { self.progress.resize(n, false); }
    }

    fn step_met(&self) -> bool {
        self.current().is_some_and(|s| s.complete.met(&mut self.progress.iter()))
    }

    /// Latch every leaf of the current step that `signal` (or, with `None`, the remembered state and the
    /// clock) satisfies.
    fn latch(&mut self, signal: Option<&Signal>) {
        self.fit_progress();
        let Some(step) = self.script.steps.get(self.step) else { return };
        let age = self.step_age();
        let mem = &self.mem;
        let progress = &mut self.progress;
        // Kits are matched as a multiset: each placed kit fills at most one leaf.
        let mut free_kits: Vec<&KitId> = match signal {
            Some(Signal::KitPlaced { kit }) => vec![kit],
            Some(_) => Vec::new(),
            None => mem.kits.iter().collect(),
        };
        // Kits already credited to latched leaves of this step are not free again.
        if signal.is_none() {
            let mut base = 0;
            step.complete.for_each_leaf(&mut base, &mut |i, leaf| {
                if let (Leaf::Kit(k), true) = (leaf, progress.get(i).copied().unwrap_or(false)) {
                    if let Some(pos) = free_kits.iter().position(|p| kit_matches(k, p)) { free_kits.remove(pos); }
                }
            });
        }
        let mut base = 0;
        step.complete.for_each_leaf(&mut base, &mut |i, leaf| {
            let Some(slot) = progress.get_mut(i) else { return };
            if *slot { return; }
            *slot = match leaf {
                Leaf::Kit(k) => match free_kits.iter().position(|p| kit_matches(k, p)) {
                    Some(pos) => { free_kits.remove(pos); true }
                    None => false,
                },
                Leaf::Other(t) => leaf_met(t, signal, mem, age),
            };
        });
    }

    /// Enter the step at `self.step` (or finish), auto-completing steps whose condition already holds.
    fn enter(&mut self, out: &mut Vec<Effect>) {
        for _ in 0..MAX_CHAIN {
            if self.step >= self.script.steps.len() {
                self.finish(out);
                return;
            }
            self.step_started_s = self.now();
            self.step_signal_s = 0;
            self.progress = vec![false; self.script.steps[self.step].complete.leaf_count()];
            let step = &self.script.steps[self.step];
            out.extend(step.on_enter.iter().filter(|e| !matches!(e, Effect::Reward(_) | Effect::Finished | Effect::Log { .. })).cloned());
            out.push(Effect::Allow(step.allow.clone()));
            out.push(Effect::Show { text: step.text.clone(), point_at: step.point_at.clone() });
            self.latch(None);
            if !self.step_met() { return; }
            self.leave(out);
        }
        // A script that chains past the bound is malformed; end it rather than spin.
        self.finish(out);
    }

    /// Close the current step: assists, log row, then move the cursor on (the caller enters the next one).
    fn leave(&mut self, out: &mut Vec<Effect>) {
        let Some(step) = self.script.steps.get(self.step) else { return };
        let now = self.now();
        let timed_out = !step.complete.met_without_timers(&self.progress);
        if timed_out {
            out.extend(step.on_escape.iter().filter(|e| !matches!(e, Effect::Reward(_) | Effect::Finished | Effect::Log { .. })).cloned());
        }
        let mut notes = Vec::new();
        step.complete.note_leaves(&mut 0, &mut notes);
        for (i, note) in notes {
            if !self.progress.get(i).copied().unwrap_or(false) && !self.mem.fixed.contains(&note) {
                self.mem.fixed.insert(note.clone());
                out.push(Effect::ApplyNoteFix(note));
            }
        }
        let (id, fight, kill) = (step.id.clone(), step.marks_first_fight, step.marks_first_kill);
        if timed_out {
            self.log.timed_out.push(id.clone());
            out.push(Effect::Log { step: format!("{id}.timed_out"), seconds: now });
        }
        if fight && self.log.first_fight_s.is_none() { out.push(Effect::Log { step: "first_fight_missed".into(), seconds: now }); }
        if kill && self.log.first_kill_s.is_none() { out.push(Effect::Log { step: "first_kill_missed".into(), seconds: now }); }
        self.log.steps.push((id.clone(), now));
        out.push(Effect::Log { step: id, seconds: now });
        self.step += 1;
    }

    fn finish(&mut self, out: &mut Vec<Effect>) {
        if self.finished { return; }
        self.finished = true;
        self.step = self.script.steps.len();
        self.progress.clear();
        self.log.finished_s = Some(self.now());
        if !self.script.reward.is_empty() { out.push(Effect::Reward(self.script.reward.clone())); }
        out.push(Effect::Finished);
    }

    /// Remember edge-triggered state and take the gate stamps.
    fn observe(&mut self, signal: &Signal, out: &mut Vec<Effect>) {
        let now = self.now();
        match signal {
            Signal::Gauge { gauge, pass } => { self.mem.gauges.insert(gauge.clone(), *pass); }
            Signal::KitPlaced { kit } => self.mem.kits.push(kit.clone()),
            Signal::NoteFixApplied { note } => { self.mem.fixed.insert(note.clone()); }
            Signal::Elapsed { seconds } => self.step_signal_s = self.step_signal_s.max(*seconds),
            Signal::PlayerGunsFired if self.log.first_fight_s.is_none() => {
                self.log.first_fight_s = Some(now);
                out.push(Effect::Log { step: "first_fight".into(), seconds: now });
            }
            Signal::EnemyDestroyed { .. } if self.log.first_kill_s.is_none() => {
                self.log.first_kill_s = Some(now);
                out.push(Effect::Log { step: "first_kill".into(), seconds: now });
            }
            _ => {}
        }
    }
}

/// Whether a non-kit leaf is met by `signal` (or, with `None`, by remembered state and the clock alone).
fn leaf_met(t: &Trigger, signal: Option<&Signal>, mem: &Remembered, age: f64) -> bool {
    if let Trigger::After { seconds } | Trigger::Dwell { seconds } = t { return age >= *seconds as f64; }
    match signal {
        None => match t {
            Trigger::GaugePass(g) => mem.gauges.get(g).copied().unwrap_or(false),
            // A "no notes open" report counts only when it arrives during the step: any earlier one predates
            // the last edit, and the blocked arc appears only once both the gun and the mast are placed.
            Trigger::NoteFixed(n) => mem.fixed.contains(n),
            _ => false,
        },
        Some(s) => match (t, s) {
            (Trigger::Continue, Signal::Continue) => true,
            (Trigger::GaugePass(g), Signal::Gauge { gauge, pass: true }) => g == gauge,
            (Trigger::NoteFixed(n), Signal::NoteFixApplied { note }) => n == note,
            (Trigger::NoteFixed(_), Signal::NotesOpen { count: 0 }) => true,
            // The note is not open on the design as it stands: nothing is left to fix (or it never came up).
            (Trigger::NoteFixed(n), Signal::OpenNotes { ids }) => !ids.iter().any(|i| i == n),
            (Trigger::SplashStarted, Signal::SplashStarted) => true,
            (Trigger::PlayerGunsFired, Signal::PlayerGunsFired) => true,
            (Trigger::EnemyDestroyed, Signal::EnemyDestroyed { .. }) => true,
            (Trigger::SliderMoved(k), Signal::SliderMoved { key }) => k == key,
            (Trigger::HelmInput, Signal::HelmInput) => true,
            _ => false,
        },
    }
}

/// Every translation key the embedded scripts use, sorted and without repeats (the CI check that each key
/// has a row in `game/i18n/tutorial.csv`).
pub fn text_keys() -> Vec<String> {
    let keys: BTreeSet<String> = scripts().iter().flat_map(|s| s.steps.iter()).flat_map(|st| {
        let mut ks = vec![st.text.0.clone()];
        ks.extend(st.done.iter().map(|d| d.0.clone()));
        ks.extend(st.on_enter.iter().filter_map(|e| match e { Effect::Show { text, .. } => Some(text.0.clone()), _ => None }));
        ks
    }).collect();
    keys.into_iter().collect()
}

/// Seconds as "12.3 s (0:12)".
fn fmt_s(s: f64) -> String {
    let s = clean(s);
    let whole = s as u64;
    format!("{s:.1} s ({}:{:02})", whole / 60, whole % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::settings::SplashTarget;

    fn kit(s: &str) -> KitId { KitId::new(s) }
    fn step(id: &str, complete: Trigger) -> Step {
        Step { id: id.into(), text: TextKey::new(format!("tutorial.{id}")), point_at: None, allow: vec![], on_enter: vec![], on_escape: vec![], complete, marks_first_fight: false, marks_first_kill: false, lesson: 0, done: None }
    }
    fn script(steps: Vec<Step>) -> Script { Script { id: "t".into(), fallback_level: 0, blueprint: "b".into(), reward: "r".into(), steps, spots: vec![] } }
    fn finished(effects: &[Effect]) -> bool { effects.contains(&Effect::Finished) }
    fn shows(effects: &[Effect], key: &str) -> bool { effects.iter().any(|e| matches!(e, Effect::Show { text, .. } if text.as_str() == key)) }

    fn walk(t: &Trigger, f: &mut dyn FnMut(&Trigger)) {
        f(t);
        if let Trigger::All(ts) | Trigger::Any(ts) = t { ts.iter().for_each(|c| walk(c, f)); }
    }
    fn contains(t: &Trigger, pred: &dyn Fn(&Trigger) -> bool) -> bool {
        let mut hit = false;
        walk(t, &mut |c| hit |= pred(c));
        hit
    }

    /// The signals a cooperative player produces for one trigger, in a sensible order.
    fn ideal(t: &Trigger, out: &mut Vec<Signal>) {
        match t {
            Trigger::Continue => out.push(Signal::Continue),
            Trigger::GaugePass(g) => out.push(Signal::Gauge { gauge: g.clone(), pass: true }),
            Trigger::KitsPlaced(ks) => out.extend(ks.iter().map(|k| Signal::KitPlaced { kit: k.clone() })),
            Trigger::NoteFixed(n) => out.push(Signal::NoteFixApplied { note: n.clone() }),
            Trigger::SplashStarted => out.push(Signal::SplashStarted),
            Trigger::PlayerGunsFired => out.push(Signal::PlayerGunsFired),
            Trigger::EnemyDestroyed => out.push(Signal::EnemyDestroyed { class: None }),
            Trigger::SliderMoved(k) => out.push(Signal::SliderMoved { key: k.clone() }),
            Trigger::HelmInput => out.push(Signal::HelmInput),
            Trigger::After { seconds } | Trigger::Dwell { seconds } => out.push(Signal::Elapsed { seconds: *seconds }),
            Trigger::All(ts) => ts.iter().for_each(|c| ideal(c, out)),
            // The first branch that is not a timed escape, as a real player would take it.
            Trigger::Any(ts) => if let Some(c) = ts.iter().find(|c| !matches!(c, Trigger::After { .. })).or(ts.first()) { ideal(c, out) },
        }
    }

    #[test]
    fn embedded_data_parses_with_one_script_per_level() {
        content::data::parse_ron::<Vec<Script>>(TUTORIAL_DATA, TUTORIAL_FILE).expect("tutorial.ron parses");
        let all = scripts();
        assert_eq!(all.iter().map(|s| s.fallback_level).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
        for level in 0..=4 { assert_eq!(script_for(level).map(|s| s.fallback_level), Some(level)); }
        assert!(script_for(5).is_none());
    }

    #[test]
    fn shipped_scripts_follow_the_rules() {
        for s in scripts() {
            let first = &s.steps[0];
            assert_eq!(first.on_enter.first(), Some(&Effect::LoadBlueprint(s.blueprint.clone())), "{}: first step loads the hull", s.id);
            assert_eq!(s.reward, "starters/fac22.bp.ron");
            let ids: BTreeSet<&str> = s.steps.iter().map(|st| st.id.as_str()).collect();
            assert_eq!(ids.len(), s.steps.len(), "{}: step ids are unique", s.id);
            let fights: Vec<&Step> = s.steps.iter().filter(|st| st.marks_first_fight).collect();
            assert_eq!(fights.len(), 1, "{}: one first-fight mark", s.id);
            assert!(contains(&fights[0].complete, &|t| *t == Trigger::PlayerGunsFired));
            let kills: Vec<&Step> = s.steps.iter().filter(|st| st.marks_first_kill).collect();
            assert_eq!(kills.len(), usize::from(s.fallback_level < 4), "{}: kill mark on every level with the patrol boat", s.id);
            assert!(kills.iter().all(|st| contains(&st.complete, &|t| *t == Trigger::EnemyDestroyed)));
            let mut fight_seen = false;
            for st in &s.steps {
                assert!(st.text.as_str().starts_with("tutorial."), "{}: key namespace", st.id);
                assert!(!st.on_enter.iter().any(|e| matches!(e, Effect::Reward(_) | Effect::Finished | Effect::Log { .. })), "{}: the run emits these", st.id);
                assert!(!contains(&st.complete, &|t| matches!(t, Trigger::Any(v) | Trigger::All(v) if v.is_empty())), "{}: no empty compound", st.id);
                // Nobody is held in place: every step ends on a press the player always has (Continue, Space)
                // or carries a timed escape at its top.
                let ok = match &st.complete {
                    Trigger::Continue | Trigger::SplashStarted => true,
                    Trigger::Any(v) => v.iter().any(|c| matches!(c, Trigger::After { .. } | Trigger::Continue)),
                    _ => false,
                };
                assert!(ok, "{}/{}: step can hold the player", s.id, st.id);
                // From the first splash on the whole editor stays usable.
                if fight_seen || st.marks_first_fight { assert!(st.allow.is_empty(), "{}/{}: allow empty after the first splash", s.id, st.id); }
                fight_seen |= st.marks_first_fight;
                if matches!(st.complete, Trigger::Continue) && !fight_seen { assert!(st.allow.iter().all(|a| a.starts_with("tutorial.")), "{}: text steps keep the editor inert", st.id); }
                for e in &st.on_enter {
                    if let Effect::SplashOptions(o) = e { assert_eq!(o.water, content::weather::SeaState::CALM); }
                }
                assert!(!st.on_escape.iter().any(|e| matches!(e, Effect::Reward(_) | Effect::Finished | Effect::Log { .. })), "{}: the run emits these", st.id);
                // A kit step that times out hands over the fitted hull, so the fight steps stay playable.
                if contains(&st.complete, &|t| matches!(t, Trigger::KitsPlaced(_))) {
                    assert_eq!(st.on_escape, vec![Effect::LoadBlueprint(s.reward.clone())], "{}/{}: kit step assist", s.id, st.id);
                }
            }
            // Fallback contents (L271-274).
            let has = |pred: &dyn Fn(&Trigger) -> bool| s.steps.iter().any(|st| contains(&st.complete, pred));
            let kits: Vec<KitId> = s.steps.iter().flat_map(|st| { let mut v = Vec::new(); walk(&st.complete, &mut |t| if let Trigger::KitsPlaced(k) = t { v.extend(k.iter().cloned()) }); v }).collect();
            match s.fallback_level {
                0 => { assert!(has(&|t| matches!(t, Trigger::GaugePass(g) if g == "float"))); assert_eq!(kits.len(), 4); }
                1 => { assert!(!has(&|t| matches!(t, Trigger::GaugePass(_)))); assert_eq!(kits.len(), 4); }
                2 | 3 => { assert!(!has(&|t| matches!(t, Trigger::GaugePass(_)))); assert_eq!(kits, vec![kit("kit.gun"), kit("kit.radar_mast")]); }
                _ => { assert_eq!(kits, vec![kit("kit.gun")]); assert!(!has(&|t| matches!(t, Trigger::NoteFixed(_)))); }
            }
            let demo = |name: &str| s.steps.iter().any(|st| st.on_enter.contains(&Effect::Demo(name.into())));
            assert_eq!(demo("tutorial.demo.beam_handle"), (1..=3).contains(&s.fallback_level), "{}", s.id);
            assert_eq!(demo("tutorial.demo.fix_ghost_preview"), s.fallback_level == 3, "{}", s.id);
            let targets: Vec<SplashTarget> = s.steps.iter().flat_map(|st| st.on_enter.iter()).filter_map(|e| match e { Effect::SplashOptions(o) => Some(o.target), _ => None }).collect();
            let want = if s.fallback_level < 4 { vec![SplashTarget::Drones, SplashTarget::PatrolBoat] } else { vec![SplashTarget::Drones] };
            assert_eq!(targets, want, "{}", s.id);
        }
    }

    /// Minimal CSV reader for the three-column i18n files (quoted fields, doubled quotes).
    fn csv_rows(text: &str) -> Vec<Vec<String>> {
        let mut rows = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let (mut fields, mut cur, mut quoted, mut chars) = (Vec::new(), String::new(), false, line.chars().peekable());
            while let Some(c) = chars.next() {
                match (c, quoted) {
                    ('"', true) if chars.peek() == Some(&'"') => { cur.push('"'); chars.next(); }
                    ('"', _) => quoted = !quoted,
                    (',', false) => fields.push(core::mem::take(&mut cur)),
                    _ => cur.push(c),
                }
            }
            fields.push(cur);
            rows.push(fields);
        }
        rows
    }

    #[test]
    fn every_key_has_one_english_row_and_the_copy_obeys_the_rules() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/tutorial.csv");
        let text = std::fs::read_to_string(path).expect("game/i18n/tutorial.csv");
        let rows = csv_rows(&text);
        assert_eq!(rows[0], vec!["keys", "?context", "en"]);
        let mut csv_keys = BTreeSet::new();
        let banned = ["introducing", "unlock", "empower", "elevate", "streamline", "transform", "discover", "seamless", "smart", "next-gen", "revolutionize", "leverage", "delve", "vibrant", "pivotal", "showcase", "tapestry", "testament", "unleash", "get started"];
        for r in &rows[1..] {
            assert_eq!(r.len(), 3, "row {r:?}");
            assert!(csv_keys.insert(r[0].clone()), "duplicate key {}", r[0]);
            let en = &r[2];
            let lower = en.to_lowercase();
            assert!(!en.is_empty() && !en.contains('!') && en.matches('—').count() == 0, "{}", r[0]);
            for w in banned { assert!(!lower.contains(w), "{}: banned word {w}", r[0]); }
            assert!(en.chars().next().is_some_and(char::is_uppercase), "{}: sentence starts with a capital", r[0]);
            assert!(en.ends_with('.'), "{}: a full sentence", r[0]);
        }
        let used: BTreeSet<String> = text_keys().into_iter().collect();
        assert_eq!(used, csv_keys, "script keys and CSV rows match both ways");
    }

    #[test]
    fn stock_files_match_once_they_exist() {
        let stock: Vec<&str> = content::data::stock_blueprint_files().map(|(p, _)| p).collect();
        if stock.iter().any(|p| p.starts_with("tutorial/")) {
            for s in scripts() {
                assert!(stock.contains(&s.blueprint.as_str()), "{}: stock hull {} is missing", s.id, s.blueprint);
                assert!(stock.contains(&s.reward.as_str()), "{}: reward {} is missing", s.id, s.reward);
            }
        }
        let kits: Vec<KitId> = content::data::stock_kit_files().filter_map(|(p, text)| content::kit::parse_kit(text, p).ok()).map(|k| k.id).collect();
        if kits.iter().any(|k| !k.as_str().starts_with("kit.example")) {
            for s in scripts() {
                for st in &s.steps {
                    walk(&st.complete, &mut |t| if let Trigger::KitsPlaced(want) = t {
                        for w in want { assert!(kits.iter().any(|k| kit_matches(w, k)), "{}: no stock kit for {w}", s.id); }
                    });
                }
            }
        }
    }

    #[test]
    fn every_level_runs_to_the_reward_for_a_cooperative_player() {
        for s in scripts() {
            let level = s.fallback_level;
            let (mut run, mut fx) = TutorialRun::start(s.clone());
            assert!(shows(&fx, s.steps[0].text.as_str()));
            for _ in 0..s.steps.len() + 1 {
                if run.finished { break; }
                let mut sig = Vec::new();
                ideal(&run.current().unwrap().complete, &mut sig);
                for x in sig { run.tick(7.5); fx.extend(run.signal(&x)); }
            }
            assert!(run.finished, "level {level} finishes");
            let n = fx.len();
            assert_eq!(&fx[n - 2..], &[Effect::Reward("starters/fac22.bp.ron".into()), Effect::Finished]);
            assert_eq!(run.log.steps.len(), s.steps.len());
            assert!(run.log.first_fight_s.is_some(), "level {level}: first fight stamped");
            // The drones are enemies too: the first one destroyed stamps the kill gate at every level.
            assert!(run.log.first_kill_s.is_some(), "level {level}: first kill stamped");
            assert!(!fx.iter().any(|e| matches!(e, Effect::ApplyNoteFix(_))) || level == 3, "only fallback 3 applies the Fix for the player");
            assert!(run.signal(&Signal::Continue).is_empty(), "signals after the finish do nothing");
            assert!(run.log.timed_out.is_empty(), "level {level}: a cooperative player never times out");
        }
    }

    #[test]
    fn every_level_finishes_for_a_player_who_only_reads_and_presses_space() {
        for s in scripts() {
            let (mut run, mut fx) = TutorialRun::start(s.clone());
            for _ in 0..40 * 60 {
                if run.finished { break; }
                run.tick(1.0);
                let age = (run.elapsed_s - run.step_started_s) as u32;
                fx.extend(run.signal(&Signal::Elapsed { seconds: age }));
                if run.finished { break; }
                // Only the presses that are always available.
                match run.current().map(|st| &st.complete) {
                    Some(Trigger::Continue) => fx.extend(run.signal(&Signal::Continue)),
                    Some(Trigger::SplashStarted) => fx.extend(run.signal(&Signal::SplashStarted)),
                    Some(Trigger::Any(v)) if v.contains(&Trigger::SplashStarted) => fx.extend(run.signal(&Signal::SplashStarted)),
                    _ => {}
                }
            }
            assert!(run.finished, "{} finishes within 40 minutes for an idle player", s.id);
            assert!(run.log.first_fight_s.is_none() && run.log.first_kill_s.is_none(), "escapes never stamp the gates");
            if s.fallback_level < 4 { assert!(fx.contains(&Effect::ApplyNoteFix("arc_blocked".into())), "{}: the unmet Fix is applied for the player", s.id); }
            assert!(fx.contains(&Effect::LoadBlueprint("starters/fac22.bp.ron".into())), "{}: the idle player gets the fitted hull", s.id);
            assert!(run.log.timed_out.iter().any(|id| id == "kits"), "{}: the kit step timed out", s.id);
        }
    }

    #[test]
    fn entering_a_step_shows_it_after_its_effects() {
        let mut st = step("a", Trigger::Continue);
        st.on_enter = vec![Effect::LoadBlueprint("x".into())];
        st.allow = vec!["tutorial.continue".into()];
        st.point_at = Some("editor.launch".into());
        let (run, fx) = TutorialRun::start(script(vec![st]));
        assert_eq!(fx, vec![
            Effect::LoadBlueprint("x".into()),
            Effect::Allow(vec!["tutorial.continue".into()]),
            Effect::Show { text: TextKey::new("tutorial.a"), point_at: Some("editor.launch".into()) },
        ]);
        assert_eq!(run.step, 0);
        assert!(!run.finished);
    }

    #[test]
    fn conditions_already_true_complete_on_entry() {
        let s = script(vec![
            step("open", Trigger::Continue),
            step("beam", Trigger::GaugePass("float".into())),
            step("kits", Trigger::KitsPlaced(vec![kit("kit.gun"), kit("kit.engine")])),
            step("end", Trigger::Continue),
        ]);
        let (mut run, _) = TutorialRun::start(s);
        // Reported while the text step shows: the gauge passes and one kit lands early.
        assert!(run.signal(&Signal::Gauge { gauge: "float".into(), pass: true }).is_empty());
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun.20mm") });
        let fx = run.signal(&Signal::Continue);
        assert!(shows(&fx, "tutorial.kits"), "beam step skipped straight through");
        assert_eq!(run.current().unwrap().id, "kits");
        assert_eq!(run.progress, vec![true, false], "the early gun counts");
        let fx = run.signal(&Signal::KitPlaced { kit: kit("kit.engine") });
        assert!(shows(&fx, "tutorial.end"));
    }

    #[test]
    fn a_gauge_that_fails_again_is_not_remembered_as_passing() {
        let s = script(vec![step("open", Trigger::Continue), step("beam", Trigger::GaugePass("float".into())), step("end", Trigger::Continue)]);
        let (mut run, _) = TutorialRun::start(s);
        run.signal(&Signal::Gauge { gauge: "float".into(), pass: true });
        run.signal(&Signal::Gauge { gauge: "float".into(), pass: false });
        run.signal(&Signal::Continue);
        assert_eq!(run.current().unwrap().id, "beam");
        assert!(run.signal(&Signal::Gauge { gauge: "speed".into(), pass: true }).is_empty());
        assert!(shows(&run.signal(&Signal::Gauge { gauge: "float".into(), pass: true }), "tutorial.end"));
    }

    #[test]
    fn kits_count_as_a_multiset_and_by_family() {
        let s = script(vec![step("kits", Trigger::KitsPlaced(vec![kit("kit.gun"), kit("kit.gun"), kit("kit.radar_mast")]))]);
        let (mut run, _) = TutorialRun::start(s);
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun.20mm") });
        assert_eq!(run.progress, vec![true, false, false], "one gun fills one leaf");
        run.signal(&Signal::KitPlaced { kit: kit("kit.gunboat") });
        assert_eq!(run.progress, vec![true, false, false], "a different family does not count");
        run.signal(&Signal::KitPlaced { kit: kit("kit.radar_mast") });
        let fx = run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        assert!(finished(&fx));
    }

    #[test]
    fn the_fix_step_ends_on_the_fix_on_no_notes_or_on_its_escape() {
        let fix = || script(vec![step("fix", Trigger::Any(vec![Trigger::NoteFixed("arc_blocked".into()), Trigger::After { seconds: 150 }])), step("end", Trigger::Continue)]);
        // The player presses Fix.
        let (mut run, _) = TutorialRun::start(fix());
        let fx = run.signal(&Signal::NoteFixApplied { note: "arc_blocked".into() });
        assert!(shows(&fx, "tutorial.end") && !fx.iter().any(|e| matches!(e, Effect::ApplyNoteFix(_))));
        // The gun went where nothing blocks it: no notes are open.
        let (mut run, _) = TutorialRun::start(fix());
        assert!(run.signal(&Signal::NotesOpen { count: 2 }).is_empty());
        let fx = run.signal(&Signal::NotesOpen { count: 0 });
        assert!(shows(&fx, "tutorial.end") && !fx.iter().any(|e| matches!(e, Effect::ApplyNoteFix(_))));
        // Nothing happens for 150 s: the tutorial applies the Fix and moves on.
        let (mut run, _) = TutorialRun::start(fix());
        run.tick(149.0);
        assert!(run.signal(&Signal::Elapsed { seconds: 149 }).is_empty());
        run.tick(1.0);
        let fx = run.signal(&Signal::NotesOpen { count: 1 });
        assert_eq!(fx[0], Effect::ApplyNoteFix("arc_blocked".into()));
        assert!(shows(&fx, "tutorial.end"));
        // A different note's Fix does not count.
        let (mut run, _) = TutorialRun::start(fix());
        assert!(run.signal(&Signal::NoteFixApplied { note: "float_fail".into() }).is_empty());
    }

    #[test]
    fn a_stale_no_notes_report_does_not_skip_the_fix_step() {
        let s = script(vec![
            step("kits", Trigger::KitsPlaced(vec![kit("kit.radar_mast"), kit("kit.gun")])),
            step("fix_arc", Trigger::Any(vec![Trigger::NoteFixed("arc_blocked".into()), Trigger::After { seconds: 150 }])),
            step("end", Trigger::Continue),
        ]);
        let (mut run, _) = TutorialRun::start(s);
        run.signal(&Signal::KitPlaced { kit: kit("kit.radar_mast") });
        run.signal(&Signal::NotesOpen { count: 0 });
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        assert_eq!(run.current().unwrap().id, "fix_arc", "the zero predates the gun");
        assert!(run.signal(&Signal::NotesOpen { count: 1 }).is_empty());
        let fx = run.signal(&Signal::NoteFixApplied { note: "arc_blocked".into() });
        assert!(shows(&fx, "tutorial.end") && !fx.iter().any(|e| matches!(e, Effect::ApplyNoteFix(_))));
    }

    #[test]
    fn a_fix_step_ends_when_its_note_is_not_open_on_the_design() {
        let s = || script(vec![
            step("kits", Trigger::KitsPlaced(vec![kit("kit.gun")])),
            step("fix_arc", Trigger::Any(vec![Trigger::NoteFixed("arc_blocked".into()), Trigger::After { seconds: 150 }])),
            step("end", Trigger::Continue),
        ]);
        // The recompute after the gun shows the blocked arc: the step waits for the Fix.
        let (mut run, _) = TutorialRun::start(s());
        run.signal(&Signal::OpenNotes { ids: vec!["no_director".into()] });
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        assert_eq!(run.current().unwrap().id, "fix_arc", "a list from before the gun does not count");
        assert!(run.signal(&Signal::OpenNotes { ids: vec!["arc_blocked".into(), "no_director".into()] }).is_empty());
        let fx = run.signal(&Signal::NoteFixApplied { note: "arc_blocked".into() });
        assert!(shows(&fx, "tutorial.end") && !fx.iter().any(|e| matches!(e, Effect::ApplyNoteFix(_))));
        // The player's gun went where nothing blocks it: the first recompute ends the step, with nothing applied.
        let (mut run, _) = TutorialRun::start(s());
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        let fx = run.signal(&Signal::OpenNotes { ids: vec!["no_director".into()] });
        assert!(shows(&fx, "tutorial.end") && !fx.iter().any(|e| matches!(e, Effect::ApplyNoteFix(_))));
        assert!(run.log.timed_out.is_empty());
    }

    #[test]
    fn the_card_reads_kit_progress_lessons_and_what_was_just_done() {
        let mut beam = step("beam", Trigger::Any(vec![Trigger::GaugePass("float".into()), Trigger::After { seconds: 60 }]));
        beam.lesson = 1;
        beam.done = Some(TextKey::new("tutorial.done.beam"));
        let mut kits = step("kits", Trigger::Any(vec![Trigger::KitsPlaced(vec![kit("kit.gun"), kit("kit.radar_mast")]), Trigger::After { seconds: 60 }]));
        kits.lesson = 2;
        kits.done = Some(TextKey::new("tutorial.done.kits"));
        let end = step("end", Trigger::Continue);
        let s = script(vec![beam, kits, end]);
        assert_eq!(s.lessons(), 2);
        let (mut run, _) = TutorialRun::start(s.clone());
        assert!(run.previous_done().is_none() && !run.wants_kits());
        assert_eq!(run.kits_progress(), (vec![], vec![]));
        run.signal(&Signal::Gauge { gauge: "float".into(), pass: true });
        assert_eq!(run.previous_done().map(|k| k.as_str()), Some("tutorial.done.beam"));
        assert!(run.wants_kits());
        assert_eq!(run.kits_progress(), (vec![kit("kit.gun"), kit("kit.radar_mast")], vec![]));
        run.signal(&Signal::KitPlaced { kit: kit("kit.radar_mast") });
        assert_eq!(run.kits_progress(), (vec![kit("kit.gun")], vec![kit("kit.radar_mast")]));
        // A step that ended through its escape confirms nothing.
        run.tick(61.0);
        run.signal(&Signal::Elapsed { seconds: 61 });
        assert_eq!(run.current().unwrap().id, "end");
        assert!(run.previous_done().is_none());
        assert!(!run.wants_kits());
    }

    #[test]
    fn shipped_scripts_number_six_lessons_and_place_every_lesson_kit() {
        for s in scripts() {
            let numbered: Vec<u8> = s.steps.iter().map(|st| st.lesson).filter(|l| *l > 0).collect();
            assert!(numbered.windows(2).all(|w| w[1] == w[0] || w[1] == w[0] + 1), "{}: lessons count up by one", s.id);
            assert_eq!(numbered.first(), Some(&1), "{}", s.id);
            assert_eq!(s.lessons(), if s.fallback_level < 4 { 6 } else { 3 }, "{}", s.id);
            assert_eq!(s.steps.last().map(|st| st.lesson), Some(0), "{}: the closing step is unnumbered", s.id);
            for st in &s.steps {
                let mut kits = Vec::new();
                walk(&st.complete, &mut |t| if let Trigger::KitsPlaced(k) = t { kits.extend(k.iter().cloned()) });
                for k in kits { assert!(s.spot_for(&k).is_some(), "{}/{}: {k} has no spot", s.id, st.id); }
            }
        }
    }

    #[test]
    fn a_dwell_holds_the_step_open_without_counting_as_an_escape() {
        let t = Trigger::Any(vec![Trigger::All(vec![Trigger::PlayerGunsFired, Trigger::Any(vec![Trigger::EnemyDestroyed, Trigger::Dwell { seconds: 30 }])]), Trigger::After { seconds: 300 }]);
        let (mut run, _) = TutorialRun::start(script(vec![step("drones", t.clone()), step("end", Trigger::Continue)]));
        run.tick(3.0);
        run.signal(&Signal::PlayerGunsFired);
        assert_eq!(run.current().unwrap().id, "drones", "the guns alone do not end it");
        run.tick(27.0);
        assert!(shows(&run.signal(&Signal::Elapsed { seconds: 30 }), "tutorial.end"));
        assert!(run.log.timed_out.is_empty(), "a dwell is not an escape");
        // A drone down ends it at once; with no shot at all only the escape does, and that is logged.
        let (mut run, _) = TutorialRun::start(script(vec![step("drones", t.clone()), step("end", Trigger::Continue)]));
        run.signal(&Signal::PlayerGunsFired);
        assert!(shows(&run.signal(&Signal::EnemyDestroyed { class: None }), "tutorial.end"));
        let (mut run, _) = TutorialRun::start(script(vec![step("drones", t), step("end", Trigger::Continue)]));
        run.tick(300.0);
        run.signal(&Signal::Elapsed { seconds: 300 });
        assert_eq!(run.log.timed_out, vec!["drones".to_string()]);
    }

    #[test]
    fn a_stale_elapsed_report_does_not_fire_the_next_escape() {
        let s = script(vec![step("a", Trigger::After { seconds: 10 }), step("b", Trigger::Any(vec![Trigger::HelmInput, Trigger::After { seconds: 10 }])), step("end", Trigger::Continue)]);
        let (mut run, _) = TutorialRun::start(s);
        run.tick(10.0);
        assert!(shows(&run.signal(&Signal::Elapsed { seconds: 10 }), "tutorial.b"));
        assert!(run.signal(&Signal::Elapsed { seconds: 10 }).is_empty(), "the old count belongs to step a");
        run.tick(10.0);
        assert!(shows(&run.signal(&Signal::Elapsed { seconds: 10 }), "tutorial.end"));
    }

    #[test]
    fn timed_leaves_follow_either_clock() {
        let s = || script(vec![step("demo", Trigger::After { seconds: 20 }), step("end", Trigger::Continue)]);
        let (mut run, _) = TutorialRun::start(s());
        assert!(run.signal(&Signal::Elapsed { seconds: 19 }).is_empty());
        assert!(shows(&run.signal(&Signal::Elapsed { seconds: 20 }), "tutorial.end"));
        let (mut run, _) = TutorialRun::start(s());
        run.tick(20.5);
        assert!(shows(&run.signal(&Signal::HelmInput), "tutorial.end"), "the run's own clock counts too");
        // After(0) and empty compounds never hold anyone.
        let (run, fx) = TutorialRun::start(script(vec![step("a", Trigger::After { seconds: 0 }), step("b", Trigger::All(vec![])), step("c", Trigger::Any(vec![])), step("d", Trigger::KitsPlaced(vec![]))]));
        assert!(run.finished && finished(&fx));
        assert_eq!(run.log.steps.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn the_drone_step_needs_guns_and_steering_or_twenty_seconds() {
        let t = Trigger::Any(vec![Trigger::All(vec![Trigger::PlayerGunsFired, Trigger::Any(vec![Trigger::HelmInput, Trigger::After { seconds: 20 }])]), Trigger::After { seconds: 300 }]);
        let mut st = step("drones", t);
        st.marks_first_fight = true;
        let (mut run, _) = TutorialRun::start(script(vec![st.clone(), step("end", Trigger::Continue)]));
        assert_eq!(run.progress.len(), 4);
        run.tick(5.0);
        let fx = run.signal(&Signal::PlayerGunsFired);
        assert_eq!(fx, vec![Effect::Log { step: "first_fight".into(), seconds: 5.0 }], "stamped at the shot, step not over");
        run.tick(5.0);
        assert!(shows(&run.signal(&Signal::HelmInput), "tutorial.end"));
        assert_eq!(run.log.first_fight_s, Some(5.0));
        // Without steering, the guns alone end it once 20 s have passed.
        let (mut run, _) = TutorialRun::start(script(vec![st, step("end", Trigger::Continue)]));
        run.signal(&Signal::PlayerGunsFired);
        run.tick(21.0);
        assert!(shows(&run.signal(&Signal::Elapsed { seconds: 21 }), "tutorial.end"));
    }

    #[test]
    fn a_timed_out_step_runs_its_assist_and_is_logged() {
        let mut kits = step("kits", Trigger::Any(vec![Trigger::KitsPlaced(vec![kit("kit.gun"), kit("kit.engine")]), Trigger::After { seconds: 60 }]));
        kits.on_escape = vec![Effect::LoadBlueprint("full".into()), Effect::Finished];
        let s = script(vec![kits, step("end", Trigger::Continue)]);
        // Finished in time: no assist, nothing logged as timed out.
        let (mut run, _) = TutorialRun::start(s.clone());
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        run.tick(30.0);
        let fx = run.signal(&Signal::KitPlaced { kit: kit("kit.engine") });
        assert!(shows(&fx, "tutorial.end") && !fx.contains(&Effect::LoadBlueprint("full".into())));
        assert!(run.log.timed_out.is_empty());
        // One kit short when the clock runs out: the assist runs first, and `Finished` is not passed through.
        let (mut run, _) = TutorialRun::start(s);
        run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        run.tick(60.0);
        let fx = run.signal(&Signal::Elapsed { seconds: 60 });
        assert_eq!(fx[0], Effect::LoadBlueprint("full".into()));
        assert_eq!(fx[1], Effect::Log { step: "kits.timed_out".into(), seconds: 60.0 });
        assert!(shows(&fx, "tutorial.end") && !finished(&fx));
        assert_eq!(run.log.timed_out, vec!["kits".to_string()]);
        assert!(log_text(&run.log).contains("Timed out: kits"));
    }

    #[test]
    fn bad_input_never_panics() {
        // An empty script finishes at once.
        let (mut run, fx) = TutorialRun::start(script(vec![]));
        assert!(run.finished && fx == vec![Effect::Reward("r".into()), Effect::Finished]);
        assert!(run.signal(&Signal::Continue).is_empty());
        assert!(run.allows("editor.anything"));
        // Junk time and a tampered run.
        let (mut run, _) = TutorialRun::start(script(vec![step("a", Trigger::KitsPlaced(vec![kit("kit.gun")])), step("b", Trigger::Continue)]));
        run.tick(f64::NAN);
        run.tick(-5.0);
        run.tick(f64::INFINITY);
        assert_eq!(run.elapsed_s, 0.0);
        run.progress = vec![false; 9];
        run.signal(&Signal::Elapsed { seconds: u32::MAX });
        assert_eq!(run.progress.len(), 1);
        run.elapsed_s = f64::NAN;
        run.step_started_s = f64::NEG_INFINITY;
        let fx = run.signal(&Signal::KitPlaced { kit: kit("kit.gun") });
        assert!(shows(&fx, "tutorial.b"));
        assert!(fx.iter().all(|e| !matches!(e, Effect::Log { seconds, .. } if !seconds.is_finite())));
        run.step = 99;
        let fx = run.signal(&Signal::Continue);
        assert!(finished(&fx) && run.finished);
        // Every signal kind on a shipped script, in a hostile order, at once.
        let junk = [Signal::SplashEnded, Signal::Designated, Signal::PartPlaced { part: PartId::new("x") }, Signal::HandleChanged { handle: String::new() }, Signal::NotesOpen { count: u16::MAX }, Signal::Gauge { gauge: String::new(), pass: true }, Signal::SliderMoved { key: "\n".into() }, Signal::EnemyDestroyed { class: Some(ClassId::new("c")) }];
        for s in scripts() {
            let (mut run, _) = TutorialRun::start(s);
            for _ in 0..3 { for j in &junk { run.signal(j); } }
            assert!(!run.finished, "junk alone does not finish a script");
        }
    }

    #[test]
    fn allow_lists_match_by_dotted_prefix() {
        let mut st = step("kits", Trigger::Continue);
        st.allow = vec!["editor.tray".into()];
        assert!(st.allows("editor.tray") && st.allows("editor.tray.kit.gun"));
        assert!(!st.allows("editor.trayx") && !st.allows("editor.launch") && !st.allows("editor"));
        assert!(st.allows("tutorial.continue") && st.allows("editor.camera.orbit") && st.allows("menu"));
        st.allow.clear();
        assert!(st.allows("editor.launch"), "empty allows everything");
        let (run, _) = TutorialRun::start(script(vec![{ let mut s = step("a", Trigger::Continue); s.allow = vec!["tutorial.continue".into()]; s }]));
        assert!(!run.allows("editor.handle.beam") && run.allows("tutorial.continue"));
    }

    #[test]
    fn simulation_events_map_to_signals() {
        use crate::protocol::command::ControlRole;
        use sim_core::PhysEvent;
        let me = EntityId::from_u64(3);
        let other = EntityId::from_u64(7);
        let fired = |v| SimEvent::Phys(PhysEvent::MountFired { vessel: v, slot: Default::default(), rounds: 1 });
        assert_eq!(signal_from_event(&fired(me), me), Some(Signal::PlayerGunsFired));
        assert_eq!(signal_from_event(&fired(other), me), None);
        assert_eq!(signal_from_event(&fired(me), EntityId::NONE), None);
        let lost = |v, by| SimEvent::Phys(PhysEvent::VesselLost { vessel: v, side: Default::default(), cause: Default::default(), by, tonnage_t: 30.0 });
        assert_eq!(signal_from_event(&lost(other, EntityId::NONE), me), Some(Signal::EnemyDestroyed { class: None }));
        assert_eq!(signal_from_event(&lost(me, other), me), None, "the player's own loss is not a kill");
        let shot = |by, down| SimEvent::Phys(PhysEvent::BodyDestroyed { body: other, pos: DVec3::ZERO, shot_down: down, by });
        assert_eq!(signal_from_event(&shot(me, true), me), None, "a missile shot down is not a target beaten");
        assert_eq!(signal_from_event(&shot(me, false), me), None);
        assert_eq!(signal_from_event(&shot(other, true), me), None);
        // A target out of the fight but afloat (colours struck, propulsion gone) is beaten; the player's own is not.
        let status = |v, status| SimEvent::Phys(PhysEvent::StatusChanged { vessel: v, status });
        assert_eq!(signal_from_event(&status(other, sim_core::vessel::VesselStatus::Disabled), me), Some(Signal::EnemyDestroyed { class: None }));
        assert_eq!(signal_from_event(&status(me, sim_core::vessel::VesselStatus::Disabled), me), None);
        assert_eq!(signal_from_event(&status(other, sim_core::vessel::VesselStatus::Active), me), None);
        let helm = |holder| SimEvent::ControlChanged { vessel: me, role: ControlRole::Helm, holder };
        assert_eq!(signal_from_event(&helm(ControlHolder::Player), me), Some(Signal::HelmInput));
        assert_eq!(signal_from_event(&helm(ControlHolder::Officer), me), None);
        assert_eq!(signal_from_event(&SimEvent::EntityHidden { entity: me }, me), None);
    }

    #[test]
    fn the_log_reads_as_plain_text_and_survives_a_round_trip() {
        let s = script_for(0).unwrap();
        let (mut run, _) = TutorialRun::start(s);
        run.tick(42.0);
        run.signal(&Signal::Continue);
        run.tick(30.0);
        run.signal(&Signal::Gauge { gauge: "float".into(), pass: true });
        let text = log_text(&run.log);
        assert!(text.contains("Script: first_ten_minutes (fallback level 0)"));
        assert!(text.contains("open: 42.0 s (0:42), 42.0 s (0:42)"));
        assert!(text.contains("beam: 72.0 s (1:12), 30.0 s (0:30)"));
        assert!(text.contains("Time to first fight: not reached"));
        assert!(text.contains("Timed out: none"));
        assert!(text.lines().all(|l| !l.is_empty()));
        let back: TutorialRun = bincode::deserialize(&bincode::serialize(&run).unwrap()).unwrap();
        assert_eq!(back, run);
        let mut log = TutorialLog { script: "a\nb".into(), first_fight_s: Some(f64::NAN), ..Default::default() };
        log.steps.push(("x\ty".into(), 3725.0));
        let text = log_text(&log);
        assert!(text.contains("Script: a b") && text.contains("x y: 3725.0 s (62:05)") && text.contains("first fight: 0.0 s"));
    }

    #[test]
    fn note_ids_are_snake_case_and_distinct() {
        use crate::editor::notes::NoteKind as K;
        assert_eq!(note_id(&K::ArcBlocked { from_deg: 0.0, to_deg: 20.0, blocked_fraction: 0.6 }), "arc_blocked");
        let all = [K::FloatFail, K::NoPropulsion, K::NoGeneration, K::NoSteering, K::Unpowered, K::Unconnected, K::NoMagazine, K::NoDirector, K::OverTonnage, K::OverComplexity, K::Advisory(String::new()), K::GaugeCaution(String::new()), K::RollsOver { sea_state: 6, from_deg: 60.0 }];
        let ids: BTreeSet<&str> = all.iter().map(note_id).collect();
        assert_eq!(ids.len(), all.len());
        assert!(ids.iter().all(|i| i.chars().all(|c| c.is_ascii_lowercase() || c == '_')));
    }
}
