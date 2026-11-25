//! Unit `gd-editor-core`. `BpEditor` (autoload `Editor`): the dry dock's editing model. This file holds the
//! class, its signals, the single undo history, tools, symmetry, selection and the clipboard.
//! DESIGN.md "The editor" (L118-276): L233 (mirror and radial symmetry, drag-to-place, multi-select with
//! copy and paste, one undo history across hull, kits, tuning and graphs, prefab capture with version and
//! author lineage, non-drag alternatives for every drag), L58 (keyboard, mouse and trackpad only).
//! Sibling units add `#[godot_api(secondary)]` blocks: `hull.rs` = `gd-editor-hull`, `parts.rs` =
//! `gd-editor-parts`, `control.rs` = `gd-editor-control`, `graph.rs` = `gd-editor-graph`,
//! `analysis.rs` = `gd-editor-analysis`, `io.rs` = `gd-editor-io`, `splash.rs` = `gd-editor-splash`.
//! `view.rs` = `gd-editor-view` is a separate class (`BpEditorView`).
//!
//! # Model (frozen)
//! The blueprint under edit is one `content::blueprint::Blueprint` value. Every change goes through
//! [`EditModel::apply`]: it pushes the previous value on the undo stack (changes inside `begin_group` and
//! `end_group` share one step), bumps the revision, regenerates what the design derives (routes, graph),
//! and emits `design_changed`. Headless functions in `sim_systems::editor` and `content` do the computing;
//! slow ones run on the background worker (`crate::worker`) and come back through `process`.
//! The model lives in an autoload so it survives the trip dry dock to Splash and back.
//!
//! The 3D side (meshes, ghost, overlays, picking, camera) is `BpEditorView`; it reads this model and never
//! changes it except through the methods here.
//!
//! # The commit path (for every sibling unit)
//! A sibling that changes the design calls [`BpEditor::commit`] (a new blueprint value) or
//! [`BpEditor::commit_edit`] (a `sim_systems::editor::BlueprintEdit`). Both apply through `EditModel`, then
//! [`BpEditor::announce`]: it emits `design_changed`, queues the fast gauges on the worker (the derived design:
//! grid, prefab, routes, graph) and resets the idle timer. A unit that changes `model` some other way calls
//! `announce` itself; `process` announces any revision that was missed, one frame late at worst.
//! Revisions only ever grow, across undo, redo, cancelled groups, loads and reopening, so a late worker result
//! can never pass for the current design. Other helpers: [`BpEditor::install_design`] (load or new design),
//! [`BpEditor::mark_saved`] (after a write: caches the design in `Services::design` and starts the trial
//! battery), [`BpEditor::submit_job`] (any worker job, with its `job_changed` state), and the results the
//! worker returned in [`EditorState::results`].
//!
//! # Signals
//! Signals are emitted last in every method, with no `Services` borrow held: a GDScript handler may call
//! straight back into `Editor` (gdext re-borrows through `base_mut`, so that is safe).
//!
//! # Frames
//! Editor render space (`HandleDict.pos`, `SelectionDict.centre`) is the design's vessel-local frame in
//! metres: +X bow, +Y up, +Z starboard, origin at the minimum corner of the blueprint's cell grid, so cell
//! `(x, y, z)` has its centre at `((x + 0.5) * 0.5, (y + 0.5) * 0.5, (z + 0.5) * 0.5)`. `BpEditorView` draws in
//! the same frame.
pub mod analysis;
pub mod control;
pub mod graph;
pub mod hull;
pub mod io;
pub mod parts;
pub mod share_file;
pub mod splash;
pub mod view;

use crate::convert::{d, msg_dict};
use crate::enums::{EditorMode, EditorTool, GdEnum, JobState, Symmetry};
use content::blueprint::Blueprint;
use content::cell::CellPos;
use content::ids::{ContentHash, KitSlot, PartSlot};
use godot::prelude::*;
use sim_systems::editor::gauges::GaugeStrip;
use sim_systems::editor::notes::ShipNote;
use sim_systems::protocol::{Msg, MsgArg};
use sim_systems::ship::ShipDesign;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

/// Undo depth kept per session.
pub const UNDO_DEPTH: usize = 256;

/// The design last saved in the dry dock with its built form: what Splash launches, what the lobby fields and
/// what a campaign refit commits. Shared through `Services::design`.
pub struct CurrentDesign {
    pub blueprint: Arc<Blueprint>,
    pub hash: ContentHash,
    pub design: Option<Arc<ShipDesign>>,
    pub path: String,
    /// A splash world kept ready by the worker (pre-built splash, L221), if any.
    pub prepared: Option<Arc<sim_systems::editor::splash::PreparedSplash>>,
    pub complexity_points: f64,
}

/// What the dry dock was opened for.
#[derive(Clone, Debug, PartialEq)]
pub struct DockContext {
    pub mode: EditorMode,
    pub ruleset: content::ruleset::Ruleset,
    pub act: content::enums::Act,
    pub cap_t: f64,
    /// Campaign: the briefing Fit Check runs against, and the fielded design a refit is costed from.
    pub brief: Option<Arc<sim_systems::editor::fitcheck::FitBrief>>,
    pub fielded: Option<Arc<Blueprint>>,
    /// Parts the player may place (`None` = every part of the build), and the SEXTANT parts store.
    pub available: Option<Arc<Vec<content::ids::PartId>>>,
    pub parts_store: Arc<std::collections::BTreeMap<content::ids::PartId, u16>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection { pub parts: Vec<PartSlot>, pub kits: Vec<KitSlot>, pub cells: Vec<content::cell::CellPos> }

/// The ghost that follows the cursor while placing or previewing a Fix.
#[derive(Clone, Debug, PartialEq)]
pub enum Ghost {
    Part { part: content::ids::PartId, cell: content::cell::CellPos, rot: content::kit::PartRot },
    Kit { kit: content::ids::KitId, cell: content::cell::CellPos, rot: content::kit::PartRot },
    /// Clipboard contents being pasted.
    Paste { offset: content::cell::CellPos },
    /// A previewed edit (Ship's Note Fix, Suggest, Quartermaster) as the resulting blueprint.
    Edit(Box<Blueprint>),
}

/// The editing model. One per process.
pub struct EditModel {
    pub blueprint: Blueprint,
    pub rev: u64,
    pub saved_rev: u64,
    pub path: String,
    pub context: DockContext,
    pub undo: Vec<(Blueprint, sim_systems::protocol::Msg)>,
    pub redo: Vec<(Blueprint, sim_systems::protocol::Msg)>,
    pub group: Option<(Blueprint, sim_systems::protocol::Msg)>,
    pub tool: EditorTool,
    pub symmetry: Symmetry,
    pub radial_count: u8,
    pub selection: Selection,
    pub clipboard: Option<Box<content::kit::KitDef>>,
    pub ghost: Option<Ghost>,
    /// Latest results from the worker with the revision they were computed for.
    pub design: Option<(u64, Arc<ShipDesign>)>,
    pub gauges: Option<(u64, Box<GaugeStrip>)>,
    pub notes: (u64, Vec<ShipNote>),
}

impl EditModel {
    /// Commit a new blueprint value as one undo step named `label` (a text key), or fold it into the open
    /// group. Clears redo, bumps `rev`, drops stale previews. Unit: `gd-editor-core`.
    ///
    /// A value equal to the current one changes nothing: no step, no revision.
    pub fn apply(&mut self, next: Blueprint, label: &str) {
        if next == self.blueprint { return; }
        let prev = std::mem::replace(&mut self.blueprint, next);
        if self.group.is_none() {
            self.undo.push((prev, Msg::new(label)));
            if self.undo.len() > UNDO_DEPTH {
                let cut = self.undo.len() - UNDO_DEPTH;
                self.undo.drain(..cut);
            }
        }
        self.redo.clear();
        self.changed();
    }
    /// Apply a mechanical edit from `sim_systems::editor` (Fix, Suggest, Quartermaster) as one undo step.
    /// False when the edit no longer applies. Unit: `gd-editor-core`.
    pub fn apply_edit(&mut self, edit: &sim_systems::editor::BlueprintEdit, label: &str, catalogue: &content::catalogue::Catalogue) -> bool {
        match sim_systems::editor::notes::apply_edit(&self.blueprint, edit, catalogue) {
            Some(next) => {
                self.apply(next, label);
                true
            }
            None => false,
        }
    }
    pub fn is_dirty(&self) -> bool { self.rev != self.saved_rev }

    /// A fresh model for a design. `rev` must be above every revision handed out before (see
    /// [`BpEditor::next_rev`]); `saved` marks it clean (a file just loaded).
    pub(crate) fn new(blueprint: Blueprint, path: String, context: DockContext, rev: u64, saved: bool) -> EditModel {
        EditModel {
            blueprint,
            rev,
            saved_rev: if saved { rev } else { rev.saturating_sub(1) },
            path,
            context,
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            tool: EditorTool::Select,
            symmetry: Symmetry::Mirror,
            radial_count: 2,
            selection: Selection::default(),
            clipboard: None,
            ghost: None,
            design: None,
            gauges: None,
            notes: (0, Vec::new()),
        }
    }

    /// Every change of the value under edit: a new revision, previews that showed the old value dropped.
    fn changed(&mut self) {
        self.rev += 1;
        if matches!(self.ghost, Some(Ghost::Edit(_))) { self.ghost = None; }
    }

    /// Replace the value without an undo step (the design waterline following the Float gauge, L130).
    pub(crate) fn replace_in_place(&mut self, next: Blueprint) {
        if next == self.blueprint { return; }
        self.blueprint = next;
        self.changed();
    }

    /// Step back. Returns false with nothing to undo.
    pub(crate) fn undo_step(&mut self) -> bool {
        self.close_group(false);
        let Some((prev, label)) = self.undo.pop() else { return false };
        let cur = std::mem::replace(&mut self.blueprint, prev);
        self.redo.push((cur, label));
        self.changed();
        true
    }
    pub(crate) fn redo_step(&mut self) -> bool {
        self.close_group(false);
        let Some((next, label)) = self.redo.pop() else { return false };
        let cur = std::mem::replace(&mut self.blueprint, next);
        self.undo.push((cur, label));
        self.changed();
        true
    }
    /// Open a group; a group already open stays (nested drags fold into the outer one).
    pub(crate) fn open_group(&mut self, label: &str) {
        if self.group.is_none() { self.group = Some((self.blueprint.clone(), Msg::new(label))); }
    }
    /// Close the open group: one undo step when it changed anything; `cancel` restores the value it started
    /// from. Returns true when the value under edit changed (a cancel that rolled back).
    pub(crate) fn close_group(&mut self, cancel: bool) -> bool {
        let Some((start, label)) = self.group.take() else { return false };
        if start == self.blueprint { return false; }
        if cancel {
            self.blueprint = start;
            self.changed();
            return true;
        }
        self.undo.push((start, label));
        if self.undo.len() > UNDO_DEPTH {
            let cut = self.undo.len() - UNDO_DEPTH;
            self.undo.drain(..cut);
        }
        self.redo.clear();
        false
    }
    /// Drop selected slots the design no longer has.
    pub(crate) fn prune_selection(&mut self) -> bool {
        let (np, nk) = (self.blueprint.parts.len(), self.blueprint.kits.len());
        let before = self.selection.clone();
        self.selection.parts.retain(|s| s.index() < np);
        self.selection.kits.retain(|k| k.index() < nk);
        self.selection != before
    }
}

// ---------------------------------------------------------------------------------------------------------
// State next to the frozen fields.

/// Results the worker returned that the analysis, splash and view units expose, each with its revision.
#[derive(Default)]
pub(crate) struct Results {
    /// The last trial battery and the revision of the saved design it ran on.
    pub(crate) scorecard: Option<(u64, Arc<sim_systems::editor::trials::Scorecard>)>,
    pub(crate) fit: Option<(u64, Arc<sim_systems::editor::fitcheck::FitResult>)>,
    pub(crate) quartermaster: Option<(u64, Arc<sim_systems::editor::quartermaster::QmResult>)>,
    /// A pre-built splash world (L221), its revision and how long it took.
    pub(crate) prepared: Option<(u64, Arc<sim_systems::editor::splash::PreparedSplash>, f32)>,
    /// Fallback note order (L198): measured effects, the revision measured.
    pub(crate) note_effects: Option<(u64, Vec<(ShipNote, f64)>)>,
    /// Revisions the fast and the slow gauges in `EditModel::gauges` were computed for.
    pub(crate) fast_rev: u64,
    pub(crate) slow_rev: u64,
    /// Worker compute times of the last fast and slow gauge jobs, milliseconds.
    pub(crate) fast_ms: f32,
    pub(crate) slow_ms: f32,
    /// The last failure message per job name.
    pub(crate) failures: BTreeMap<&'static str, String>,
}

/// Gauge latency over the last 50 edits and the automatic split strip (L185).
#[derive(Default)]
pub(crate) struct GaugeLatency {
    /// When each revision was committed, oldest first (bounded).
    commits: VecDeque<(u64, std::time::Instant)>,
    /// Edit commit to fast gauges ready, milliseconds, newest last; at most [`LATENCY_WINDOW`].
    pub(crate) samples: VecDeque<f64>,
    /// The split strip is on.
    pub(crate) split_strip: bool,
    /// Consecutive edits under 70 ms while the split strip is on.
    fast_run: u32,
}
const LATENCY_WINDOW: usize = 50;
const SPLIT_ON_MS: f64 = 100.0;
const SPLIT_OFF_MS: f64 = 70.0;
impl GaugeLatency {
    fn committed(&mut self, rev: u64) {
        self.commits.push_back((rev, std::time::Instant::now()));
        while self.commits.len() > 64 { self.commits.pop_front(); }
    }
    /// Record the fast gauges of `rev` arriving now.
    fn arrived(&mut self, rev: u64) {
        let Some(&(_, t)) = self.commits.iter().find(|(r, _)| *r == rev) else { return };
        self.commits.retain(|(r, _)| *r > rev);
        self.record(t.elapsed().as_secs_f64() * 1_000.0);
    }
    pub(crate) fn record(&mut self, ms: f64) {
        self.samples.push_back(ms);
        while self.samples.len() > LATENCY_WINDOW { self.samples.pop_front(); }
        if self.split_strip {
            self.fast_run = if ms < SPLIT_OFF_MS { self.fast_run + 1 } else { 0 };
            if self.fast_run >= LATENCY_WINDOW as u32 {
                self.split_strip = false;
                self.fast_run = 0;
            }
        } else if self.samples.len() >= LATENCY_WINDOW && self.p95() > SPLIT_ON_MS {
            self.split_strip = true;
            self.fast_run = 0;
        }
    }
    /// p95 of the window, nearest rank; 0 with no samples.
    pub(crate) fn p95(&self) -> f64 {
        if self.samples.is_empty() { return 0.0; }
        let mut v: Vec<f64> = self.samples.iter().copied().collect();
        v.sort_by(|a, b| a.total_cmp(b));
        let rank = ((0.95 * v.len() as f64).ceil() as usize).clamp(1, v.len());
        v[rank - 1]
    }
}

/// Everything `BpEditor` keeps besides the frozen fields. Sibling units read and write the parts named for
/// them; each field says who writes it.
#[derive(Default)]
pub(crate) struct EditorState {
    /// The model while the dock is closed (`close` keeps unsaved work for the next `open`). Writer: core.
    pub(crate) parked: Option<Box<EditModel>>,
    /// Highest revision handed out so far. Writer: core.
    pub(crate) rev_high: u64,
    /// The revision `design_changed` was last emitted and the fast gauges queued for. Writer: core.
    pub(crate) announced: u64,
    /// First revision of the design under edit: worker results from before it belong to a replaced design.
    pub(crate) first_rev: u64,
    /// The worker's results. Writer: core (`process`); siblings read.
    pub(crate) results: Results,
    /// Writer: core. Read by `gauge_latency` (analysis).
    pub(crate) latency: GaugeLatency,
    /// A handle drag is under way: gauges use the coarse GZ curve until it ends (L187). Writer: hull.
    pub(crate) dragging: bool,
    /// The fast gauges of `announced` were queued at drag resolution; the group's end requeues them in full.
    pub(crate) coarse_rev: u64,
    /// The blueprint of the announced revision, shared with worker jobs. Writer: core.
    pub(crate) current: Option<(u64, Arc<Blueprint>)>,
    /// Parts the last load's migrations removed (each a Ship's Note). Writer: core (`install_design`).
    pub(crate) removed_parts: Vec<content::migrate::RemovedPart>,
    /// Lobby ruleset id (`RulesetDict.id`); empty outside a lobby. Writer: core.
    pub(crate) ruleset_id: String,
    /// Campaign Salvage on hand when the dock opened; -1 elsewhere. Writer: core.
    pub(crate) budget_salvage: i64,
    /// Options and seed of the next splash, for the pre-build (L221). Writer: splash; default calm, drones.
    pub(crate) splash_options: content::settings::SplashOptions,
    pub(crate) splash_seed: u64,
    /// Revision the last pre-build was queued for. Writer: core.
    pub(crate) prebuild_rev: u64,
    /// On-save battery spec: the short trial is the L227 fallback. Writer: analysis.
    pub(crate) short_trials: bool,
    /// The L198 fallback order is on: measure each Fix after every save. Writer: analysis.
    pub(crate) note_effects_on: bool,
    /// Check state per gauge last reported to the tutorial. Writer: core.
    checks: BTreeMap<&'static str, sim_systems::editor::Check>,
    /// Open-note count last reported to the tutorial.
    notes_open: Option<u16>,
    /// The waterline followed the Float gauge at this revision (one automatic move per edit). Writer: core.
    waterline_rev: u64,
    /// Static stats of the design under edit, by revision.
    stats: std::cell::RefCell<Option<(u64, content::stats::BlueprintStats)>>,
    /// Hull unit caches. Writer: hull.
    pub(crate) hull: hull::HullCache,
    /// Parts unit caches (ghost verdicts, player kits, paste rotation). Writer: parts.
    pub(crate) parts: parts::PartsCache,
    /// Files unit caches (browser info, share string, lineage origin). Writer: io.
    pub(crate) io: io::IoCache,
    /// Graph unit caches (view by revision, layout, live values). Writer: graph.
    pub(crate) graph: graph::GraphCache,
    /// Analysis unit caches (validation, suggestions, arcs, balance, refit diff, scorecard history).
    /// Writer: analysis.
    pub(crate) analysis: analysis::AnalysisCache,
    /// The running splash, load timing and the last report card. Writer: splash.
    pub(crate) splash: splash::SplashState,
}

// ---------------------------------------------------------------------------------------------------------
// Campaign hand-off.

/// What a campaign dock opens with. `BpCampaign::open_dry_dock` leaves it with [`offer_campaign_dock`] and
/// then opens the dock with `Bp.DOCK_CAMPAIGN`; `open` takes it. Main thread only.
#[derive(Clone)]
pub struct CampaignDock {
    pub act: content::enums::Act,
    /// The act's hull cap (L801).
    pub cap_t: f64,
    /// The local player's fielded hull: the design a refit is costed from.
    pub fielded: Arc<Blueprint>,
    /// File it was saved as, if any.
    pub path: String,
    /// The current briefing, when the dock opens from one.
    pub brief: Option<Arc<sim_systems::editor::fitcheck::FitBrief>>,
    /// Parts the campaign has made available (`None` = every part), and SEXTANT's parts store.
    pub available: Option<Arc<Vec<content::ids::PartId>>>,
    pub parts_store: Arc<BTreeMap<content::ids::PartId, u16>>,
    /// Salvage on hand (shared pool in co-op).
    pub salvage: i64,
}

thread_local! {
    static CAMPAIGN_DOCK: std::cell::RefCell<Option<CampaignDock>> = const { std::cell::RefCell::new(None) };
}

/// Leave the campaign's dock context for the next `Editor.open(Bp.DOCK_CAMPAIGN, ...)`.
pub fn offer_campaign_dock(dock: CampaignDock) { CAMPAIGN_DOCK.with(|c| *c.borrow_mut() = Some(dock)); }

// ---------------------------------------------------------------------------------------------------------
// Pure helpers.

/// Text keys this unit emits (`game/i18n/dock.csv`); hull and parts list theirs beside their code.
const CORE_KEYS: &[&str] = &[
    "dock.undo.undo", "dock.undo.redo", "dock.undo.rename", "dock.undo.author", "dock.undo.delete", "dock.undo.edit",
"dock.issue.no_campaign", "dock.issue.bad_mode", "dock.issue.part_removed",
    "dock.issue.kind_changed", "dock.prefab.default_name",
];

/// Every `dock.*` key the Rust side can emit, for the test that checks each has an English row.
pub fn text_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = CORE_KEYS.to_vec();
    keys.extend_from_slice(hull::HULL_KEYS);
    keys.extend_from_slice(parts::PARTS_KEYS);
    keys.extend_from_slice(io::IO_KEYS);
    keys.extend_from_slice(control::CONTROL_KEYS);
    keys.extend_from_slice(graph::GRAPH_KEYS);
    keys.extend_from_slice(analysis::ANALYSIS_KEYS);
    keys.extend_from_slice(splash::SPLASH_KEYS);
    keys.extend_from_slice(view::VIEW_KEYS);
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// Lobby rulesets by id, from the embedded data.
pub(crate) fn lobby_rulesets() -> &'static [(String, content::ruleset::Ruleset)] {
    static LIST: std::sync::OnceLock<Vec<(String, content::ruleset::Ruleset)>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| content::data::parse_ron::<Vec<(String, content::ruleset::Ruleset)>>(content::data::RULESETS, "crates/content/data/rulesets.ron").unwrap_or_default())
}

/// The ruleset and cap a dock mode works under (see `open`).
pub(crate) fn mode_rules(mode: EditorMode, act: Option<content::enums::Act>, lobby: Option<&content::ruleset::Ruleset>) -> (content::ruleset::Ruleset, f64) {
    use content::ruleset::{Ruleset, RulesetKind};
    use content::enums::Act;
    match mode {
        EditorMode::Lobby => {
            let mut r = lobby.cloned().unwrap_or_default();
            if let Some(a) = act { r.tonnage_cap_t = a.hull_cap_t(); }
            let cap = r.tonnage_cap_t;
            (r, cap)
        }
        EditorMode::Campaign => {
            let a = act.unwrap_or(Act::Act1);
            (Ruleset::campaign(a), a.hull_cap_t())
        }
        EditorMode::Tutorial => (Ruleset::campaign(Act::Act1), Act::Act1.hull_cap_t()),
        EditorMode::EnemyClass => {
            let cap = act.map_or(0.0, |a| a.hull_cap_t());
            let r = Ruleset { kind: RulesetKind::Open, tonnage_cap_t: cap, complexity_per_vessel: 0.0, complexity_per_lobby: 0.0, max_length_m: content::cell::MAX_ENEMY_HULL_M, luau: true, ..Ruleset::default() };
            (r, cap)
        }
        EditorMode::Sandbox => {
            let a = act.unwrap_or(Act::Act4);
            let mut r = lobby.cloned().unwrap_or(Ruleset { kind: RulesetKind::Open, luau: true, ..Ruleset::default() });
            r.tonnage_cap_t = a.hull_cap_t();
            let cap = r.tonnage_cap_t;
            (r, cap)
        }
    }
}

/// The highest control layer a ruleset allows (`limits().max_layer`).
pub(crate) fn max_layer(r: &content::ruleset::Ruleset) -> content::enums::ControlLayer {
    if r.luau { content::enums::ControlLayer::L4Script } else { r.kind.max_layer() }
}

/// Longest hull a mode allows (L126).
pub(crate) fn max_length_m(mode: EditorMode, r: &content::ruleset::Ruleset) -> f64 {
    if mode == EditorMode::EnemyClass { return content::cell::MAX_ENEMY_HULL_M; }
    let m = r.max_length_m;
    if m.is_finite() { m.clamp(content::cell::MIN_PLAYER_HULL_M, content::cell::MAX_PLAYER_HULL_M) } else { content::cell::MAX_PLAYER_HULL_M }
}

/// Length of a blueprint as validation measures it: the shell length, or the structure's extent.
pub(crate) fn design_length_m(bp: &Blueprint) -> f64 {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for row in &bp.cells.0 {
        for run in &row.runs {
            if run.cell.is_empty() || run.len == 0 { continue; }
            lo = lo.min(run.x as i32);
            hi = hi.max(run.x as i32 + run.len as i32);
        }
    }
    let cells = if hi > lo { (hi - lo) as f64 * content::cell::CELL_M } else { 0.0 };
    match &bp.hull.source {
        content::hull::HullSource::Lofted(s) if s.length_m.is_finite() => cells.max(s.length_m),
        _ => cells,
    }
}

/// Stock path form (`starters/fac22.bp.ron`) of a `stock:` path or a bare stock path.
pub(crate) fn stock_key(path: &str) -> Option<String> {
    let p = path.trim();
    if p.starts_with("user://") || p.starts_with("res://") || p.is_empty() { return None; }
    let p = p.strip_prefix("stock:").unwrap_or(p).replace('\\', "/");
    let p = p.trim_start_matches("./");
    Some(p.strip_prefix("blueprints/").unwrap_or(p).to_string())
}

/// Starter a new sandbox, lobby or campaign design opens on: the first starter a player meets (L151).
const DEFAULT_STARTER: &str = "starters/fac22.bp.ron";
/// The tutorial's opening hull (L254).
const TUTORIAL_STARTER: &str = "tutorial/fac22_tutorial.bp.ron";
/// Length of a new enemy class with no `length_m` given.
const DEFAULT_CLASS_LENGTH_M: f64 = 40.0;
/// Most characters kept of a player-typed name or author.
pub(crate) const MAX_NAME_CHARS: usize = 64;

/// A player-typed name: trimmed, control characters dropped, at most [`MAX_NAME_CHARS`] characters.
pub(crate) fn clean_name(s: &str) -> String { s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(MAX_NAME_CHARS).collect() }

/// Plain-data reading of `open`'s options.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct OpenOptions { pub blueprint: Option<String>, pub ruleset: Option<String>, pub act: Option<content::enums::Act>, pub length_m: Option<f64> }
fn read_open_options(dict: &VarDictionary) -> OpenOptions {
    use crate::convert::{get_f64, get_i64, get_text};
    OpenOptions {
        blueprint: get_text(dict, "blueprint").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        ruleset: get_text(dict, "ruleset").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        act: get_i64(dict, "act").and_then(content::enums::Act::from_gd),
        length_m: get_f64(dict, "length_m"),
    }
}

fn issue(text: Msg, fatal: bool) -> VarDictionary { d! { "text" => msg_dict(&text), "fatal" => fatal, "part" => -1i64, "cell" => Variant::nil() } }

/// Read a blueprint file through Godot (`user://`, `res://`) or the stock set.
pub(crate) fn read_blueprint(path: &str) -> Result<content::blueprint::io::LoadedBlueprint, content::ContentError> {
    if let Some(key) = stock_key(path) {
        let bp = content::campaign::load::stock_blueprint(&key)?;
        return Ok(content::blueprint::io::LoadedBlueprint { from_format: bp.header.format, blueprint: bp, removed_parts: Vec::new(), rehashed: false });
    }
    let gpath = GString::from(path);
    if !godot::classes::FileAccess::file_exists(&gpath) {
        return Err(content::ContentError::Io(format!("{path}: no such file")));
    }
    let len = godot::classes::FileAccess::open(&gpath, godot::classes::file_access::ModeFlags::READ).map(|f| f.get_length()).unwrap_or(0);
    if len as usize > content::blueprint::MAX_RON_BYTES {
        return Err(content::ContentError::Limit("blueprint over 16 MiB"));
    }
    let text = godot::classes::FileAccess::get_file_as_string(&gpath).to_string();
    if text.is_empty() { return Err(content::ContentError::Io(format!("{path}: empty or unreadable"))); }
    let file = path.rsplit('/').next().unwrap_or(path).to_string();
    content::blueprint::io::load_blueprint(&text, &file)
}

/// Mirror plane sum `S` of a design (a cell `z` mirrors to `S - z`). A lofted hull's centreline is the lattice
/// plane of its shell layout; a baked hull's is the mid-plane of its rows at or below the design waterline,
/// which superstructure (a bridge wing on one side) cannot move. `None` for a design with no structure.
pub(crate) fn mirror_sum(bp: &Blueprint) -> Option<i32> {
    if let content::hull::HullSource::Lofted(s) = &bp.hull.source {
        let l = content::hull::shell_layout(s);
        if l.half_width_cells > 0 { return Some(2 * l.half_width_cells as i32 - 1); }
    }
    let extent = |low_only: bool| {
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for row in &bp.cells.0 {
            if low_only && row.y > bp.hull.design_waterline_y { continue; }
            if row.runs.iter().any(|r| !r.cell.is_empty() && r.len > 0) {
                lo = lo.min(row.z as i32);
                hi = hi.max(row.z as i32);
            }
        }
        (hi >= lo).then_some(lo + hi)
    };
    extent(true).or_else(|| extent(false))
}

/// X extent sum of the hull (radial symmetry turns about the hull's vertical centre line): the shell
/// layout's length for a lofted hull, else the rows at or below the design waterline.
pub(crate) fn x_sum(bp: &Blueprint) -> Option<i32> {
    if let content::hull::HullSource::Lofted(s) = &bp.hull.source {
        let l = content::hull::shell_layout(s);
        if l.size[0] > 0 { return Some(l.size[0] as i32 - 1); }
    }
    let extent = |low_only: bool| {
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for row in &bp.cells.0 {
            if low_only && row.y > bp.hull.design_waterline_y { continue; }
            for r in &row.runs {
                if r.cell.is_empty() || r.len == 0 { continue; }
                lo = lo.min(r.x as i32);
                hi = hi.max(r.x as i32 + r.len as i32 - 1);
            }
        }
        (hi >= lo).then_some(lo + hi)
    };
    extent(true).or_else(|| extent(false))
}

/// Clipboard and prefab body from a set of placed parts: each part's offset from the set's minimum corner.
pub(crate) fn kit_parts_from(bp: &Blueprint, slots: &[PartSlot], cat: &content::catalogue::Catalogue) -> Vec<content::kit::KitPart> {
    let mut lo = [i32::MAX; 3];
    let mut any = false;
    for s in slots {
        let Some(p) = bp.parts.get(s.index()) else { continue };
        if cat.part(&p.part).is_none() { continue; }
        lo = [lo[0].min(p.pos.x as i32), lo[1].min(p.pos.y as i32), lo[2].min(p.pos.z as i32)];
        any = true;
    }
    if !any { return Vec::new(); }
    slots
        .iter()
        .filter_map(|s| bp.parts.get(s.index()))
        .filter(|p| cat.part(&p.part).is_some())
        .map(|p| content::kit::KitPart { part: p.part.clone(), offset: CellPos::new((p.pos.x as i32 - lo[0]) as i16, (p.pos.y as i32 - lo[1]) as i16, (p.pos.z as i32 - lo[2]) as i16), rot: p.rot })
        .collect()
}

/// Selected part slots, with every part of the selected kits, ascending and unique.
pub(crate) fn selected_slots(bp: &Blueprint, sel: &Selection) -> Vec<PartSlot> {
    let mut set: BTreeSet<PartSlot> = sel.parts.iter().copied().filter(|s| s.index() < bp.parts.len()).collect();
    for k in &sel.kits {
        if let Some(kit) = bp.kits.get(k.index()) {
            set.extend(kit.parts.iter().copied().filter(|s| s.index() < bp.parts.len()));
        }
    }
    set.into_iter().collect()
}

/// A kit id from a player-typed name: `player.` and the name in lower case with runs of other characters as
/// one `_`, at most 48 characters.
pub(crate) fn kit_id_from_name(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() { slug.push(c); } else if !slug.ends_with('_') && !slug.is_empty() { slug.push('_'); }
        if slug.len() >= 48 { break; }
    }
    let slug = slug.trim_matches('_');
    format!("player.{}", if slug.is_empty() { "kit" } else { slug })
}

/// A prefab from a selection (L233): version 1, the author lineage of its source (the one kit all the parts
/// came from, else the design), parts relative to the selection's corner. `None` when it would hold no part.
pub(crate) fn make_prefab(bp: &Blueprint, slots: &[PartSlot], id: &str, name: &str, author: &str, cat: &content::catalogue::Catalogue) -> Option<content::kit::KitDef> {
    use content::kit::{KitDef, KitKind, LineageEntry, MAX_LINEAGE};
    let parts = kit_parts_from(bp, slots, cat);
    if parts.is_empty() { return None; }
    // The source: one kit instance holding exactly these parts, else the design itself.
    let kits: BTreeSet<Option<KitSlot>> = slots.iter().filter_map(|s| bp.parts.get(s.index())).map(|p| p.kit).collect();
    let source_kit = match kits.iter().next() {
        Some(Some(k)) if kits.len() == 1 => bp.kits.get(k.index()).filter(|kp| kp.parts.len() == slots.len()).and_then(|kp| bp.embedded_kits.get(&kp.kit).or_else(|| cat.kit(&kp.kit))),
        _ => None,
    };
    let mut lineage = match source_kit {
        Some(def) => {
            let mut l = def.lineage.clone();
            l.push(LineageEntry { hash: content::kit::kit_hash(def), author: def.author.clone().unwrap_or_default() });
            l
        }
        None => {
            let mut l = bp.header.lineage.clone();
            l.push(LineageEntry { hash: content::blueprint::io::compute_hash(bp), author: bp.header.author.clone().unwrap_or_default() });
            l
        }
    };
    if lineage.len() > MAX_LINEAGE { lineage.drain(..lineage.len() - MAX_LINEAGE); }
    let mut tags: Vec<content::part::PartTag> = parts.iter().filter_map(|p| cat.part(&p.part)).flat_map(|d| d.tags.iter().copied()).collect();
    tags.sort_by_key(|t| format!("{t:?}"));
    tags.dedup();
    let (settings, officer, graph) = match source_kit { Some(def) => (def.settings.clone(), def.officer.clone(), def.graph.clone()), None => (Vec::new(), None, Default::default()) };
    let mut kit = KitDef {
        format: content::kit::KIT_FORMAT,
        id: content::ids::KitId::new(id),
        version: 1,
        hash: ContentHash::EMPTY,
        name: clean_name(name),
        name_key: None,
        description_key: None,
        author: (!author.trim().is_empty()).then(|| clean_name(author)),
        licence: content::blueprint::DEFAULT_LICENCE.to_string(),
        lineage,
        kind: KitKind::Assembly,
        parts,
        sockets: Vec::new(),
        settings,
        graph,
        officer,
        tags,
        tray_order: u16::MAX,
    };
    content::kit::canonicalize_kit(&mut kit);
    kit.hash = content::kit::kit_hash(&kit);
    Some(kit)
}

// ---------------------------------------------------------------------------------------------------------
// The class.

/// The dry dock model. Autoload `Editor`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpEditor {
    pub(crate) model: Option<Box<EditModel>>,
    /// Worker results waiting to be applied, and job states by name.
    pub(crate) jobs: std::collections::BTreeMap<&'static str, crate::enums::JobState>,
    pub(crate) idle_s: f64,
    pub(crate) in_splash: bool,
    /// Everything else the editor keeps (see [`EditorState`]).
    pub(crate) state: EditorState,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpEditor {
    fn init(base: Base<Node>) -> Self { Self { model: None, jobs: Default::default(), idle_s: 0.0, in_splash: false, state: EditorState { budget_salvage: -1, splash_seed: 1, ..Default::default() }, base } }
    fn ready(&mut self) { self.base_mut().set_process_priority(-90); }
    /// Unit `gd-editor-core`: poll the worker, store results that match the current revision, emit
    /// `gauges_changed`, `notes_changed`, `scorecard_changed`, `fit_changed`, `quartermaster_changed` and
    /// `job_changed`; after 500 ms idle queue the splash pre-build (L221). Inert while the dock is closed.
    ///
    /// While closed it only keeps the worker informed (no worker is started) and files late results into the
    /// parked model, so a trial battery that finishes after `close` is not lost.
    fn process(&mut self, delta: f64) {
        let services = crate::services::services();
        let ticking = matches!(services.feed.borrow().state, crate::enums::SessionState::InSortie | crate::enums::SessionState::Loading);
        let mut results = Vec::new();
        if let Some(w) = services.worker.borrow().as_ref() {
            w.set_sortie_ticking(ticking);
            w.poll(&mut results);
        }
        drop(services);
        let mut emits = Emits::default();
        for r in results { self.intake(r, &mut emits); }
        // Sibling hooks: live graph values of a splash (graph.rs) and scorecard history (analysis.rs).
        self.graph_frame();
        self.analysis_frame();
        if self.model.is_some() {
            self.idle_s += if delta.is_finite() && delta > 0.0 { delta } else { 0.0 };
            self.announce();
            self.maybe_prebuild(&mut emits);
        }
        // Unit `gd-editor-splash`: load timing, what the basin shows and the end of a splash.
        self.splash_frame();
        self.flush(emits);
    }
}

/// Signals collected while handling worker results, emitted together at the end of `process`.
#[derive(Default)]
struct Emits { gauges: bool, notes: bool, scorecard: bool, fit: bool, quartermaster: bool, jobs: Vec<(&'static str, JobState)>, routed: Vec<i64> }

/// Seconds of idle before the splash pre-build (L221).
const PREBUILD_IDLE_S: f64 = 0.5;

impl BpEditor {
    // ---- Commit path (pub(crate): sibling units) ----

    /// The next revision for a replaced model: above everything handed out.
    pub(crate) fn next_rev(&mut self) -> u64 {
        let cur = self.model.as_ref().map_or(0, |m| m.rev).max(self.state.parked.as_ref().map_or(0, |m| m.rev));
        self.state.rev_high = self.state.rev_high.max(cur) + 1;
        self.state.rev_high
    }

    /// Commit a new blueprint value as one undo step (or into the open group), then [`Self::announce`].
    /// Returns false when the dock is closed or nothing changed.
    pub(crate) fn commit(&mut self, next: Blueprint, label: &str) -> bool {
        let Some(m) = self.model.as_mut() else { return false };
        let before = m.rev;
        m.apply(next, label);
        let changed = m.rev != before;
        if changed { self.announce(); }
        changed
    }

    /// Commit a `BlueprintEdit` (`sim_systems::editor::notes::apply_edit`) as one undo step. False when the
    /// dock is closed or the edit does not apply.
    pub(crate) fn commit_edit(&mut self, edit: &sim_systems::editor::BlueprintEdit, label: &str) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_mut() else { return false };
        let before = m.rev;
        if !m.apply_edit(edit, label, &cat) { return false; }
        if m.rev != before { self.announce(); }
        true
    }

    /// Tell everyone the value under edit changed, if it did since the last call: drop selections of slots
    /// that are gone, queue the fast gauges (the design rebuild: grid, prefab, routes, graph), reset the idle
    /// timer and emit `design_changed` (and `selection_changed` and `undo_changed` as needed).
    pub(crate) fn announce(&mut self) {
        let Some(m) = self.model.as_mut() else { return };
        if m.rev == self.state.announced { return; }
        let rev = m.rev;
        self.state.announced = rev;
        self.state.rev_high = self.state.rev_high.max(rev);
        self.idle_s = 0.0;
        let pruned = m.prune_selection();
        let arc = Arc::new(m.blueprint.clone());
        let cap = m.context.cap_t;
        self.state.current = Some((rev, arc.clone()));
        self.state.latency.committed(rev);
        self.state.parts.invalidate();
        self.state.hull.invalidate();
        let dragging = self.state.dragging;
        if dragging { self.state.coarse_rev = rev; }
        self.submit_job("gauges", crate::worker::Job::FastGauges { rev, blueprint: arc, cap_t: cap, dragging });
        if pruned { self.base_mut().emit_signal("selection_changed", &[]); }
        self.base_mut().emit_signal("undo_changed", &[]);
        self.base_mut().emit_signal("design_changed", &[(rev as i64).to_variant()]);
    }

    /// Queue a worker job (starting the worker on first use) and report its state as Running.
    pub(crate) fn submit_job(&mut self, name: &'static str, job: crate::worker::Job) {
        crate::services::services().with_worker(|w| w.submit(job));
        self.set_job(name, JobState::Running);
    }

    /// Record a job state and emit `job_changed`.
    pub(crate) fn set_job(&mut self, name: &'static str, state: JobState) {
        let prev = self.jobs.insert(name, state);
        if prev != Some(state) {
            self.base_mut().emit_signal("job_changed", &[StringName::from(name).to_variant(), state.to_gd().to_variant()]);
        }
    }

    /// The design built for the current revision, if the worker has returned it.
    pub(crate) fn current_design(&self) -> Option<Arc<ShipDesign>> {
        let m = self.model.as_ref()?;
        m.design.as_ref().filter(|(r, _)| *r == m.rev).map(|(_, d)| d.clone())
    }
    /// The newest design the worker built (maybe for an older revision), for views that prefer a slightly
    /// old answer to none.
    pub(crate) fn latest_design(&self) -> Option<Arc<ShipDesign>> { self.model.as_ref()?.design.as_ref().map(|(_, d)| d.clone()) }

    /// Make `bp` the design under edit (a file loaded, a new design, a tutorial hull): a fresh history and
    /// revision, older worker jobs cancelled. `saved` marks it clean.
    pub(crate) fn install_design(&mut self, bp: Blueprint, path: String, removed: Vec<content::migrate::RemovedPart>, saved: bool) -> bool {
        let Some(old) = self.model.as_ref() else { return false };
        let context = old.context.clone();
        let (tool, symmetry, radial, clipboard) = (old.tool, old.symmetry, old.radial_count, old.clipboard.clone());
        let rev = self.next_rev();
        let mut m = EditModel::new(bp, path, context, rev, saved);
        m.tool = if matches!(tool, EditorTool::PlacePart | EditorTool::PlaceKit) { EditorTool::Select } else { tool };
        m.symmetry = symmetry;
        m.radial_count = radial;
        m.clipboard = clipboard;
        let ruleset = m.context.ruleset.clone();
        self.model = Some(Box::new(m));
        self.state.first_rev = rev;
        self.state.results = Results::default();
        self.state.removed_parts = removed.clone();
        self.state.waterline_rev = 0;
        self.state.prebuild_rev = 0;
        self.state.dragging = false;
        self.state.checks.clear();
        self.state.notes_open = None;
        crate::services::services().with_worker(|w| {
            w.cancel_before(rev);
            w.set_design_context(ruleset, removed);
        });
        for name in ["notes", "trials", "fit", "quartermaster", "prebuild"] { self.jobs.remove(name); }
        self.announce();
        self.parts_design_installed();
        self.base_mut().emit_signal("selection_changed", &[]);
        self.base_mut().emit_signal("ghost_changed", &[]);
        true
    }

    /// After a successful write of the design under edit to `path`: it is clean, it becomes
    /// `Services::design` with its built form when the worker has one, and the trial battery starts (L227).
    pub(crate) fn mark_saved(&mut self, path: &str) {
        let Some(m) = self.model.as_mut() else { return };
        m.saved_rev = m.rev;
        m.path = path.to_string();
        let rev = m.rev;
        let mut bp = m.blueprint.clone();
        bp.header.hash = content::blueprint::io::compute_hash(&bp);
        let hash = bp.header.hash;
        let design = m.design.as_ref().filter(|(r, _)| *r == rev).map(|(_, d)| d.clone());
        let prepared = self.state.results.prepared.as_ref().filter(|(_, p, _)| p.blueprint_hash == hash).map(|(_, p, _)| p.clone());
        let points = self.state.results.scorecard.as_ref().filter(|(_, s)| s.design == hash).map_or(0.0, |(_, s)| s.complexity_points);
        let bp = Arc::new(bp);
        let current = CurrentDesign { blueprint: bp.clone(), hash, design, path: path.to_string(), prepared, complexity_points: points };
        *crate::services::services().design.borrow_mut() = Some(Arc::new(current));
        let spec = if self.state.short_trials { sim_systems::editor::trials::BatterySpec::SHORT } else { sim_systems::editor::trials::BatterySpec::ON_SAVE };
        self.submit_job("trials", crate::worker::Job::Trials { rev, blueprint: bp.clone(), spec });
        if self.state.note_effects_on { self.submit_job("note_effects", crate::worker::Job::NoteEffects { rev, blueprint: bp }); }
        self.base_mut().emit_signal("undo_changed", &[]);
        self.base_mut().emit_signal("saved", &[GString::from(path).to_variant()]);
    }

    /// Static stats of the design under edit (cached per revision).
    pub(crate) fn stats(&self) -> Option<content::stats::BlueprintStats> {
        let m = self.model.as_ref()?;
        if let Some((r, s)) = self.state.stats.borrow().as_ref() {
            if *r == m.rev { return Some(s.clone()); }
        }
        let s = content::stats::compute_stats(&m.blueprint, &crate::services::services().catalogue());
        *self.state.stats.borrow_mut() = Some((m.rev, s.clone()));
        Some(s)
    }

    // ---- Worker results ----

    /// The open model, else the parked one.
    fn model_any(&mut self) -> Option<&mut EditModel> {
        if self.model.is_some() { self.model.as_deref_mut() } else { self.state.parked.as_deref_mut() }
    }

    fn intake(&mut self, r: crate::worker::JobResult, emits: &mut Emits) {
        use crate::worker::JobResult as R;
        let rev_of = match &r {
            R::FastGauges { rev, .. } | R::SlowGaugesAndNotes { rev, .. } | R::Trials { rev, .. } | R::FitCheck { rev, .. } | R::Quartermaster { rev, .. } | R::PrebuiltSplash { rev, .. } | R::NoteEffects { rev, .. } => Some(*rev),
            R::BuiltReceived { .. } | R::Failed { .. } => None,
        };
        // A result from before the design under edit was installed belongs to the design it replaced.
        if rev_of.is_some_and(|r| r < self.state.first_rev) { return; }
        let open = self.model.is_some();
        match r {
            R::FastGauges { rev, design, gauges, millis } => {
                if open { self.intake_fast(rev, design, gauges, millis, emits); return; }
                // Closed: file it with the parked work; nothing is queued or announced.
                let fast_rev = self.state.results.fast_rev;
                let Some(m) = self.state.parked.as_deref_mut() else { return };
                if rev < fast_rev || rev > m.rev { return; }
                merge_fast(m, rev, design, &gauges);
                self.state.results.fast_rev = rev;
                self.state.results.fast_ms = millis;
            }
            R::SlowGaugesAndNotes { rev, gauges, notes, millis } => {
                if open { self.intake_slow(rev, *gauges, notes, millis, emits); return; }
                let (fast_rev, slow_rev) = (self.state.results.fast_rev, self.state.results.slow_rev);
                let effects = self.state.results.note_effects.as_ref().map(|(_, l)| l.clone()).unwrap_or_default();
                let Some(m) = self.state.parked.as_deref_mut() else { return };
                if rev < slow_rev || rev > m.rev { return; }
                merge_slow(m, rev, *gauges, notes, fast_rev, &effects);
                self.state.results.slow_rev = rev;
                self.state.results.slow_ms = millis;
            }
            R::Trials { rev, scorecard } => {
                let card: Arc<sim_systems::editor::trials::Scorecard> = Arc::from(scorecard);
                let cat = crate::services::services().catalogue();
                if let Some(m) = self.model_any() {
                    if let Some((_, strip)) = m.gauges.as_mut() {
                        strip.complexity = sim_systems::editor::gauges::complexity_gauge(card.complexity_points, &cat);
                        strip.stale.retain(|s| s != "complexity");
                        emits.gauges = true;
                    }
                }
                let services = crate::services::services();
                let mut slot = services.design.borrow_mut();
                if let Some(cur) = slot.as_ref().filter(|c| c.hash == card.design) {
                    *slot = Some(Arc::new(CurrentDesign { blueprint: cur.blueprint.clone(), hash: cur.hash, design: cur.design.clone(), path: cur.path.clone(), prepared: cur.prepared.clone(), complexity_points: card.complexity_points }));
                }
                drop(slot);
                self.state.results.scorecard = Some((rev, card));
                emits.scorecard = true;
                emits.jobs.push(("trials", JobState::Done));
            }
            R::FitCheck { rev, result } => {
                let current = self.model_any().is_some_and(|m| m.rev == rev);
                self.state.results.fit = Some((rev, Arc::from(result)));
                emits.fit = true;
                emits.jobs.push(("fit", if current { JobState::Done } else { JobState::Stale }));
                // Tier-2 notes follow the briefing's tags (L193): recompute them against the new result.
                if current && open { self.queue_slow(rev, emits); }
            }
            R::Quartermaster { rev, result } => {
                let current = self.model_any().is_some_and(|m| m.rev == rev);
                self.state.results.quartermaster = Some((rev, Arc::from(result)));
                emits.quartermaster = true;
                emits.jobs.push(("quartermaster", if current { JobState::Done } else { JobState::Stale }));
            }
            R::PrebuiltSplash { rev, prepared, millis } => {
                let services = crate::services::services();
                let mut slot = services.design.borrow_mut();
                if let Some(cur) = slot.as_ref().filter(|c| c.hash == prepared.blueprint_hash) {
                    *slot = Some(Arc::new(CurrentDesign { blueprint: cur.blueprint.clone(), hash: cur.hash, design: cur.design.clone().or_else(|| Some(prepared.design.clone())), path: cur.path.clone(), prepared: Some(prepared.clone()), complexity_points: cur.complexity_points }));
                }
                drop(slot);
                self.state.results.prepared = Some((rev, prepared, millis));
                emits.jobs.push(("prebuild", JobState::Done));
            }
            R::NoteEffects { rev, effects } => {
                let measured = crate::services::services().worker.borrow().as_ref().and_then(|w| w.measured_notes(rev));
                if let Some(notes) = measured {
                    let list: Vec<(ShipNote, f64)> = effects.iter().filter_map(|&(i, e)| notes.get(i).map(|n| (n.clone(), e))).collect();
                    if let Some(m) = self.model_any() {
                        if apply_effects(&mut m.notes.1, &list) { emits.notes = true; }
                    }
                    self.state.results.note_effects = Some((rev, list));
                }
                emits.jobs.push(("note_effects", JobState::Done));
            }
            R::BuiltReceived { .. } => {}
            R::Failed { what, message } => {
                godot_warn!("dry dock: the {what} job failed: {message}");
                self.state.results.failures.insert(what, message);
                emits.jobs.push((what, JobState::Failed));
            }
        }
    }

    fn intake_fast(&mut self, rev: u64, design: Arc<ShipDesign>, gauges: Box<GaugeStrip>, millis: f32, emits: &mut Emits) {
        let Some(m) = self.model.as_mut() else { return };
        if rev < self.state.results.fast_rev || rev > m.rev { return; }
        merge_fast(m, rev, design, &gauges);
        let current = rev == m.rev;
        self.state.results.fast_rev = rev;
        self.state.results.fast_ms = millis;
        self.refresh_stale();
        emits.gauges = true;
        if current {
            self.state.latency.arrived(rev);
            emits.jobs.push(("gauges", JobState::Done));
            self.report_checks(&gauges);
            self.queue_slow(rev, emits);
            emits.routed.extend(self.parts_design_arrived(rev));
            self.follow_waterline(rev, &gauges.float);
        } else {
            emits.jobs.push(("gauges", JobState::Stale));
        }
    }

    fn intake_slow(&mut self, rev: u64, gauges: GaugeStrip, notes: Vec<ShipNote>, millis: f32, emits: &mut Emits) {
        let effects = self.state.results.note_effects.as_ref().map(|(_, l)| l.clone()).unwrap_or_default();
        let fast_rev = self.state.results.fast_rev;
        let Some(m) = self.model.as_mut() else { return };
        if rev < self.state.results.slow_rev || rev > m.rev { return; }
        merge_slow(m, rev, gauges, notes, fast_rev, &effects);
        let current = m.rev == rev;
        let open = m.notes.1.len().min(u16::MAX as usize) as u16;
        let open_ids: Vec<String> = m.notes.1.iter().map(|n| sim_systems::tutorial::note_id(&n.kind).to_string()).collect();
        self.state.results.slow_rev = rev;
        self.state.results.slow_ms = millis;
        self.refresh_stale();
        emits.gauges = true;
        emits.notes = true;
        emits.jobs.push(("notes", if current { JobState::Done } else { JobState::Stale }));
        if current && self.state.notes_open != Some(open) {
            self.state.notes_open = Some(open);
            crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::NotesOpen { count: open });
        }
        if current {
            // Which notes are open on the design now: a tutorial step waiting on a note's Fix ends when that note is
            // not among them (fixed some other way, or never raised on this player's design).
            crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::OpenNotes { ids: open_ids });
        }
    }

    /// Mark in the strip every gauge whose value predates the current revision (drawn with the stale glyph and
    /// "Updating", L185). Complexity is stale while a trial battery runs.
    fn refresh_stale(&mut self) {
        let trials_running = self.jobs.get("trials") == Some(&JobState::Running);
        let (fast, slow) = (self.state.results.fast_rev, self.state.results.slow_rev);
        let Some(m) = self.model.as_mut() else { return };
        let rev = m.rev;
        let Some((_, strip)) = m.gauges.as_mut() else { return };
        strip.stale.clear();
        if fast < rev { strip.stale.extend(crate::worker::FAST_GAUGES.iter().map(|s| s.to_string())); }
        if slow < rev { strip.stale.extend(crate::worker::SLOW_GAUGES.iter().map(|s| s.to_string())); }
        if trials_running { strip.stale.push("complexity".to_string()); }
    }

    /// Queue the slow gauges and notes for `rev` when its design is built and its blueprint is current.
    fn queue_slow(&mut self, rev: u64, emits: &mut Emits) {
        let Some(m) = self.model.as_ref() else { return };
        let Some((drev, design)) = m.design.clone() else { return };
        let Some((crev, bp)) = self.state.current.clone() else { return };
        if drev != rev || crev != rev || m.rev != rev { return; }
        let fit = self.state.results.fit.as_ref().filter(|_| m.context.mode == EditorMode::Campaign).map(|(_, f)| f.clone());
        let cap = m.context.cap_t;
        let job = crate::worker::Job::SlowGaugesAndNotes { rev, design, blueprint: bp, cap_t: cap, fit };
        crate::services::services().with_worker(|w| w.submit(job));
        emits.jobs.push(("notes", JobState::Running));
    }

    /// Tutorial: a gauge that changed check state (L254 step 2 waits for Float to pass).
    fn report_checks(&mut self, g: &GaugeStrip) {
        let now = [("float", g.float.check), ("speed", g.speed.check), ("power", g.power.check), ("punch", g.punch.check)];
        let services = crate::services::services();
        for (name, check) in now {
            if self.state.checks.insert(name, check) != Some(check) {
                services.tutorial_signal(sim_systems::tutorial::Signal::Gauge { gauge: name.to_string(), pass: check == sim_systems::editor::Check::Pass });
            }
        }
    }

    /// The design waterline follows the Float gauge (L130): when the hull floats on an even keel its
    /// waterline row moves to where the water actually is, and the boot-topping band and antifouling follow.
    /// No undo step and no unsaved-changes mark; at most once per edit, never during a drag or an open group.
    fn follow_waterline(&mut self, rev: u64, float: &sim_systems::editor::gauges::FloatGauge) {
        // Never in a Splash: a move would change the hull cells and close the basin (L216).
        if self.state.dragging || self.state.waterline_rev == rev || self.in_splash { return; }
        let Some(m) = self.model.as_mut() else { return };
        if m.rev != rev || m.group.is_some() || !float.floats || !float.draft_m.is_finite() || float.heel_deg.abs() > 5.0 || float.trim_deg.abs() > 5.0 { return; }
        let keel = m.blueprint.cells.0.iter().filter(|r| r.runs.iter().any(|run| !run.cell.is_empty() && run.len > 0)).map(|r| r.y).min().unwrap_or(0);
        let row = ((keel as f64 * content::cell::CELL_M + float.draft_m) / content::cell::CELL_M).floor();
        if !row.is_finite() || row < 0.0 || row > content::cell::MAX_GRID_DIM as f64 { return; }
        let row = row as i16;
        if row == m.blueprint.hull.design_waterline_y { return; }
        let cat = crate::services::services().catalogue();
        let Some(next) = hull::with_waterline(&m.blueprint, row, &cat) else { return };
        let clean = !m.is_dirty();
        m.replace_in_place(next);
        // An automatic move is not the player's change: a clean design stays clean.
        if clean { m.saved_rev = m.rev; }
        self.state.waterline_rev = m.rev;
    }

    fn maybe_prebuild(&mut self, emits: &mut Emits) {
        let _ = emits;
        if self.in_splash || self.idle_s < PREBUILD_IDLE_S || self.state.dragging { return; }
        let Some(m) = self.model.as_ref() else { return };
        if m.group.is_some() || self.state.prebuild_rev == m.rev { return; }
        let on = crate::services::services().settings.borrow().bool("editor.prebuilt_splash");
        if !on { return; }
        let Some((crev, bp)) = self.state.current.clone() else { return };
        if crev != m.rev { return; }
        let rev = m.rev;
        self.state.prebuild_rev = rev;
        let (options, seed) = (self.state.splash_options, self.state.splash_seed);
        self.submit_job("prebuild", crate::worker::Job::PrebuildSplash { rev, blueprint: bp, options, seed });
    }

    fn flush(&mut self, e: Emits) {
        for (name, state) in e.jobs { self.set_job(name, state); }
        for n in e.routed { self.base_mut().emit_signal("conduits_routed", &[n.to_variant()]); }
        if e.gauges { self.base_mut().emit_signal("gauges_changed", &[]); }
        if e.notes { self.base_mut().emit_signal("notes_changed", &[]); }
        if e.scorecard { self.base_mut().emit_signal("scorecard_changed", &[]); }
        if e.fit { self.base_mut().emit_signal("fit_changed", &[]); }
        if e.quartermaster { self.base_mut().emit_signal("quartermaster_changed", &[]); }
    }

    fn emit_tool_changed(&mut self) {
        let tool = self.model.as_ref().map_or(0, |m| m.tool.to_gd());
        self.base_mut().emit_signal("tool_changed", &[tool.to_variant()]);
    }

    /// Selection dictionary pieces: footprint cells, mass and cost of the selected parts.
    fn selection_summary(&self) -> (usize, Vector3, f64, i64) {
        let Some(m) = self.model.as_ref() else { return (0, Vector3::ZERO, 0.0, 0) };
        let cat = crate::services::services().catalogue();
        let slots = selected_slots(&m.blueprint, &m.selection);
        let mut cells: BTreeSet<CellPos> = m.selection.cells.iter().copied().collect();
        let (mut mass, mut cost) = (0.0f64, 0i64);
        for s in &slots {
            let p = &m.blueprint.parts[s.index()];
            let Some(def) = cat.part(&p.part) else { continue };
            if def.mass_kg.is_finite() { mass += def.mass_kg.max(0.0); }
            cost += def.cost as i64;
            cells.extend(parts::footprint_cells(def, p.pos, p.rot));
        }
        let n = cells.len();
        let centre = if n == 0 {
            Vector3::ZERO
        } else {
            let sum = cells.iter().fold([0.0f64; 3], |a, c| { let m = c.centre_m(); [a[0] + m[0], a[1] + m[1], a[2] + m[2]] });
            Vector3::new((sum[0] / n as f64) as f32, (sum[1] / n as f64) as f32, (sum[2] / n as f64) as f32)
        };
        (n, centre, mass, cost)
    }

    /// Build the model `open` installs, writing no editor state: `open` applies the result only on success.
    /// `Err` holds the fatal issue. A campaign hand-off it took is left in place again on failure.
    fn build_model(&mut self, mode: EditorMode, opts: &OpenOptions) -> Result<Opened, VarDictionary> {
        let campaign = if mode == EditorMode::Campaign {
            match CAMPAIGN_DOCK.with(|c| c.borrow_mut().take()) {
                Some(c) => Some(c),
                None => return Err(issue(Msg::new("dock.issue.no_campaign"), true)),
            }
        } else {
            None
        };
        let out = self.build_with(mode, opts, campaign.as_ref());
        if out.is_err() {
            if let Some(c) = campaign { offer_campaign_dock(c); }
        }
        out
    }

    fn build_with(&mut self, mode: EditorMode, opts: &OpenOptions, campaign: Option<&CampaignDock>) -> Result<Opened, VarDictionary> {
        let mut issues = Vec::new();
        let lobby = match mode {
            EditorMode::Lobby | EditorMode::Sandbox => opts.ruleset.as_deref().and_then(|id| lobby_rulesets().iter().find(|(k, _)| k == id)).or_else(|| (mode == EditorMode::Lobby).then(|| lobby_rulesets().first()).flatten()),
            _ => None,
        };
        let ruleset_id = lobby.map(|(k, _)| k.clone()).unwrap_or_default();
        let act = campaign.map(|c| c.act).or(opts.act);
        let (ruleset, mut cap) = mode_rules(mode, act, lobby.map(|(_, r)| r));
        let mut context = DockContext {
            mode,
            ruleset,
            act: act.unwrap_or(match mode { EditorMode::Sandbox | EditorMode::EnemyClass | EditorMode::Lobby => content::enums::Act::Act4, _ => content::enums::Act::Act1 }),
            cap_t: 0.0,
            brief: None,
            fielded: None,
            available: None,
            parts_store: Arc::new(BTreeMap::new()),
        };
        let mut budget = -1;
        if let Some(c) = campaign {
            if c.cap_t.is_finite() && c.cap_t > 0.0 { cap = c.cap_t; }
            context.ruleset.tonnage_cap_t = cap;
            context.brief = c.brief.clone();
            context.fielded = Some(c.fielded.clone());
            context.available = c.available.clone();
            context.parts_store = c.parts_store.clone();
            budget = c.salvage;
        }
        context.cap_t = cap;
        let max_len = max_length_m(mode, &context.ruleset);

        // The design: a named file, the parked model, the fielded hull, the last design, or a new one.
        let parked_ok = self.state.parked.as_ref().is_some_and(|p| p.context.mode == mode && match (&p.context.fielded, &context.fielded) { (Some(a), Some(b)) => a.header.hash == b.header.hash || Arc::ptr_eq(a, b), (None, None) => true, _ => false });
        let (mut bp, path, removed, saved) = if let Some(path) = &opts.blueprint {
            match read_blueprint(path) {
                Ok(l) => (l.blueprint, if stock_key(path).is_some() { String::new() } else { path.clone() }, l.removed_parts, true),
                Err(e) => return Err(crate::convert::error_issue_dict(&e)),
            }
        } else if parked_ok && opts.length_m.is_none() {
            let mut p = self.state.parked.take().expect("parked model");
            // Same work, the context of this opening.
            p.context = context;
            let removed = self.state.removed_parts.clone();
            return Ok(Opened { model: *p, issues, resumed: true, ruleset_id, budget, removed });
        } else if let Some(c) = campaign {
            ((*c.fielded).clone(), c.path.clone(), Vec::new(), true)
        } else if let Some(cur) = crate::services::services().design.borrow().as_ref().filter(|_| opts.length_m.is_none() && !matches!(mode, EditorMode::Tutorial | EditorMode::EnemyClass)) {
            ((*cur.blueprint).clone(), cur.path.clone(), Vec::new(), true)
        } else if mode == EditorMode::EnemyClass || opts.length_m.is_some() {
            let len = opts.length_m.unwrap_or(DEFAULT_CLASS_LENGTH_M);
            let len = if len.is_finite() { len.clamp(content::cell::MIN_PLAYER_HULL_M, max_len) } else { content::cell::MIN_PLAYER_HULL_M };
            (content::blueprint::io::new_blueprint("", len), String::new(), Vec::new(), true)
        } else {
            let starter = if mode == EditorMode::Tutorial { TUTORIAL_STARTER } else { DEFAULT_STARTER };
            match content::campaign::load::stock_blueprint(starter) {
                Ok(b) => (b, String::new(), Vec::new(), true),
                Err(e) => return Err(crate::convert::error_issue_dict(&e)),
            }
        };
        // Player modes refuse a hull over the limit and say why (L126).
        let length = design_length_m(&bp);
        if length > max_len + 1e-9 {
            let i = content::validate::Issue::TooLong { length_m: length, max_m: max_len };
            let mut dict = crate::convert::issue_dict(&i);
            dict.set("fatal", true);
            return Err(dict);
        }
        let kind = if mode == EditorMode::EnemyClass { content::blueprint::BlueprintKind::EnemyClass } else { content::blueprint::BlueprintKind::Ship };
        let mut saved = saved;
        if bp.header.kind != kind {
            bp.header.kind = kind;
            saved = false;
            issues.push(issue(Msg::new("dock.issue.kind_changed"), false));
        }
        let cat = crate::services::services().catalogue();
        for r in &removed {
            let name = cat.part(&r.part).map_or(MsgArg::Name(r.part.as_str().to_string()), |d| MsgArg::Key(d.name.clone()));
            let mut dict = issue(Msg::new("dock.issue.part_removed").arg(name), false);
            dict.set("cell", Vector3i::new(r.pos.x as i32, r.pos.y as i32, r.pos.z as i32));
            issues.push(dict);
        }
        let rev = self.next_rev();
        Ok(Opened { model: EditModel::new(bp, path, context, rev, saved), issues, resumed: false, ruleset_id, budget, removed })
    }
}

/// What `build_model` found for `open`.
struct Opened {
    model: EditModel,
    issues: Vec<VarDictionary>,
    /// The parked work for this mode, reopened (its worker results stay).
    resumed: bool,
    ruleset_id: String,
    budget: i64,
    removed: Vec<content::migrate::RemovedPart>,
}

/// Fold a fast-gauge result into a model: its design and the four fast gauges.
fn merge_fast(m: &mut EditModel, rev: u64, design: Arc<ShipDesign>, gauges: &GaugeStrip) {
    m.design = Some((rev, design));
    let strip = match m.gauges.take() {
        Some((_, mut old)) => {
            old.float = gauges.float.clone();
            old.speed = gauges.speed;
            old.power = gauges.power;
            old.punch = gauges.punch.clone();
            old
        }
        None => Box::new(gauges.clone()),
    };
    m.gauges = Some((rev, strip));
}

/// Fold a slow-gauge result into a model: the slow gauges (the whole strip when it is of the fast gauges'
/// revision, at full GZ resolution) and the notes, with measured Fix effects applied.
fn merge_slow(m: &mut EditModel, rev: u64, gauges: GaugeStrip, mut notes: Vec<ShipNote>, fast_rev: u64, effects: &[(ShipNote, f64)]) {
    let strip = match m.gauges.take() {
        Some((frev, _)) if frev == rev => gauges,
        Some((_, mut old)) => {
            old.endurance = gauges.endurance;
            old.sight = gauges.sight;
            old.resilience = gauges.resilience;
            old.complexity = gauges.complexity;
            *old
        }
        None => gauges,
    };
    m.gauges = Some((fast_rev.max(rev), Box::new(strip)));
    apply_effects(&mut notes, effects);
    m.notes = (rev, notes);
}

/// Fill measured Fix effects into notes that match a measured note, then re-rank by them (L198). Returns
/// true when any note took a value.
fn apply_effects(notes: &mut [ShipNote], measured: &[(ShipNote, f64)]) -> bool {
    if measured.is_empty() { return false; }
    let mut any = false;
    for n in notes.iter_mut() {
        if let Some((_, e)) = measured.iter().find(|(m, _)| m.kind == n.kind && m.part == n.part && m.fix == n.fix) {
            n.score_effect = Some(*e);
            any = true;
        }
    }
    if any { sim_systems::editor::notes::rank(notes, true); }
    any
}

#[godot_api]
impl BpEditor {
    /// The blueprint changed; `rev` is the new revision. Views rebuild what they show.
    #[signal]
    fn design_changed(rev: i64);
    /// `selection()` changed.
    #[signal]
    fn selection_changed();
    /// `undo_state()` changed.
    #[signal]
    fn undo_changed();
    /// The tool, symmetry or ghost changed.
    #[signal]
    fn tool_changed(tool: i64);
    #[signal]
    fn ghost_changed();
    /// `gauges()` has new values (within 100 ms of an edit for the fast gauges, L155).
    #[signal]
    fn gauges_changed();
    /// `notes()` changed.
    #[signal]
    fn notes_changed();
    /// `scorecard()` has a new result (within 60 s of a save, L227).
    #[signal]
    fn scorecard_changed();
    /// `fit_result()` changed.
    #[signal]
    fn fit_changed();
    /// `quartermaster_result()` changed.
    #[signal]
    fn quartermaster_changed();
    /// A background job changed state: `job` is `&"gauges"`, `&"notes"`, `&"trials"`, `&"fit"`,
    /// `&"quartermaster"` or `&"prebuild"`; `state` is `Bp.JOB_*`.
    #[signal]
    fn job_changed(job: StringName, state: i64);
    /// Conduits of one network (`Bp.NETWORK_*`) were re-routed after a placement; the view animates them (L138).
    #[signal]
    fn conduits_routed(network: i64);
    /// The control settings, rules or graph changed (`control.rs`, `graph.rs`).
    #[signal]
    fn control_changed();
    #[signal]
    fn graph_changed();
    /// The design was written to `path`.
    #[signal]
    fn saved(path: GString);
    /// A stock or shared kit has a newer version than `copies` placed copies (L140).
    #[signal]
    fn kit_update_offered(kit: StringName, copies: i64);
    /// Splash ended: `card` is a `ReportCardDict` (L216).
    #[signal]
    fn splash_ended(card: VarDictionary);
    /// A live edit during Splash changed the hull shell, so the basin closed (L216). `text` is a `MsgDict`.
    #[signal]
    fn returned_to_dock(text: VarDictionary);

    // ---- Opening ----

    /// Open the dry dock. `mode` is `Bp.DOCK_*`. `options` keys (all optional): `blueprint: String` (path to
    /// load; otherwise the last design or a new starter), `ruleset: String` (ruleset id, sandbox and lobby),
    /// `act: int`, `length_m: float` (new design). Campaign mode takes its cap, unlocks, parts store, fielded
    /// design and briefing from `Campaign`. Returns `Array[IssueDict]`; a fatal issue means it did not open.
    ///
    /// The design is, in order: the file `blueprint` names (`user://`, `res://` or a stock path such as
    /// `stock:starters/fac22.bp.ron`); the work left in the dock by `close` for the same mode; the campaign's
    /// fielded hull; the last saved design; a new default shell of `length_m`; or the 22 m fast attack craft
    /// (the tutorial hull in tutorial mode). Campaign mode needs `CampaignDock` from `offer_campaign_dock`.
    /// Opening an already open dock replaces its design only when `blueprint` or `length_m` is given. A design
    /// opens clean (nothing to save) until it is changed; a stock or new design has no path until saved.
    #[func]
    fn open(&mut self, mode: i64, options: VarDictionary) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(mode) = EditorMode::from_gd(mode) else {
            out.push(&issue(Msg::new("dock.issue.bad_mode"), true));
            return out;
        };
        let opts = read_open_options(&options);
        // An open dock is parked first, so the same rules decide what it opens on; it comes back if the
        // opening fails.
        let was_open = self.model.take();
        let reopen = was_open.is_some();
        let old_parked = if reopen { std::mem::replace(&mut self.state.parked, was_open) } else { None };
        let opened = match self.build_model(mode, &opts) {
            Ok(x) => x,
            Err(fatal) => {
                if reopen {
                    self.model = self.state.parked.take();
                    self.state.parked = old_parked;
                }
                out.push(&fatal);
                return out;
            }
        };
        let Opened { model, issues, resumed, ruleset_id, budget, removed } = opened;
        for i in &issues { out.push(i); }
        self.state.ruleset_id = ruleset_id;
        self.state.budget_salvage = budget;
        self.state.removed_parts = removed.clone();
        self.state.rev_high = self.state.rev_high.max(model.rev);
        let ruleset = model.context.ruleset.clone();
        let rev = model.rev;
        if !resumed {
            // A different design: its predecessor's results and running jobs go.
            self.state.first_rev = rev;
            self.state.results = Results::default();
            self.state.waterline_rev = 0;
            self.state.prebuild_rev = 0;
            self.state.checks.clear();
            self.state.notes_open = None;
            self.jobs.clear();
        }
        self.state.announced = 0;
        self.state.dragging = false;
        self.model = Some(Box::new(model));
        // Whatever was parked and not reopened is gone now.
        self.state.parked = None;
        crate::services::services().with_worker(|w| {
            if !resumed { w.cancel_before(rev); }
            w.set_design_context(ruleset, removed);
        });
        self.idle_s = 0.0;
        self.announce();
        self.parts_design_installed();
        self.base_mut().emit_signal("selection_changed", &[]);
        self.emit_tool_changed();
        self.base_mut().emit_signal("ghost_changed", &[]);
        out
    }
    /// Close the dock, keeping the model for the next `open` (unsaved changes are kept in memory).
    #[func]
    fn close(&mut self) {
        let Some(mut m) = self.model.take() else { return };
        m.close_group(false);
        m.ghost = None;
        m.selection = Selection::default();
        self.state.dragging = false;
        self.state.parked = Some(m);
        self.base_mut().emit_signal("ghost_changed", &[]);
        self.base_mut().emit_signal("selection_changed", &[]);
    }
    #[func]
    fn is_open(&self) -> bool { self.model.is_some() }
    /// `Bp.DOCK_*`.
    #[func]
    fn dock_mode(&self) -> i64 { self.model.as_ref().map_or(-1, |m| m.context.mode.to_gd()) }
    /// Current revision; increments with every committed change.
    #[func]
    fn rev(&self) -> i64 { self.model.as_ref().map_or(0, |m| m.rev as i64) }
    /// Live `BlueprintInfoDict` of the design under edit (name, size, displacement, cost, part count).
    #[func]
    fn info(&self) -> VarDictionary {
        let (Some(m), Some(stats)) = (self.model.as_ref(), self.stats()) else { return VarDictionary::new() };
        let stock = m.path.starts_with("res://");
        let mut dict = crate::convert::blueprint_info_dict(&m.blueprint, &stats, &m.path, stock, 0);
        let hash = content::blueprint::io::compute_hash(&m.blueprint);
        dict.set("hash", &crate::convert::hash_hex(&hash));
        if let Some((_, card)) = self.state.results.scorecard.as_ref().filter(|(_, c)| c.design == hash) { dict.set("complexity", card.complexity_points); }
        dict
    }
    /// Limits in force: `{ "cap_t": float, "max_length_m": float, "complexity_cap": float (0 = none),
    /// "max_layer": int (Bp.LAYER_*), "ruleset": RulesetDict, "budget_salvage": int (campaign, else -1) }`.
    #[func]
    fn limits(&self) -> VarDictionary {
        let Some(m) = self.model.as_ref() else { return VarDictionary::new() };
        let r = &m.context.ruleset;
        let complexity = if m.context.mode == EditorMode::Lobby { r.complexity_per_vessel.max(0.0) } else { 0.0 };
        let id = if self.state.ruleset_id.is_empty() { match m.context.mode { EditorMode::Campaign | EditorMode::Tutorial => "campaign", EditorMode::EnemyClass => "enemy_class", _ => "sandbox" } } else { self.state.ruleset_id.as_str() };
        d! {
            "cap_t" => m.context.cap_t,
            "max_length_m" => max_length_m(m.context.mode, r),
            "complexity_cap" => complexity,
            "max_layer" => max_layer(r).to_gd(),
            "ruleset" => crate::convert::ruleset_dict(id, r),
            "budget_salvage" => if m.context.mode == EditorMode::Campaign { self.state.budget_salvage } else { -1 },
        }
    }
    #[func]
    fn set_design_name(&mut self, name: GString) {
        let Some(m) = self.model.as_ref() else { return };
        let mut next = m.blueprint.clone();
        next.header.name = clean_name(&name.to_string());
        self.commit(next, "dock.undo.rename");
    }
    #[func]
    fn set_author(&mut self, author: GString) {
        let Some(m) = self.model.as_ref() else { return };
        let mut next = m.blueprint.clone();
        let a = clean_name(&author.to_string());
        next.header.author = (!a.is_empty()).then_some(a);
        self.commit(next, "dock.undo.author");
    }

    // ---- Undo (one history across hull, kits, tuning and graphs) ----

    #[func]
    fn undo(&mut self) -> bool {
        let Some(m) = self.model.as_mut() else { return false };
        if !m.undo_step() { return false; }
        self.state.dragging = false;
        self.announce();
        true
    }
    #[func]
    fn redo(&mut self) -> bool {
        let Some(m) = self.model.as_mut() else { return false };
        if !m.redo_step() { return false; }
        self.state.dragging = false;
        self.announce();
        true
    }
    /// Returns `UndoDict`.
    #[func]
    fn undo_state(&self) -> VarDictionary {
        let Some(m) = self.model.as_ref() else {
            return d! { "can_undo" => false, "can_redo" => false, "undo_label" => crate::convert::empty_msg(), "redo_label" => crate::convert::empty_msg(), "depth" => 0i64, "dirty" => false };
        };
        let label = |key: &str, step: Option<&(Blueprint, Msg)>| match step { Some((_, l)) => msg_dict(&Msg::new(key).arg(MsgArg::Key(l.key.clone()))), None => crate::convert::empty_msg() };
        // An open group is the next step undo takes.
        let undo_top = m.group.as_ref().filter(|(start, _)| *start != m.blueprint).or(m.undo.last());
        d! {
            "can_undo" => undo_top.is_some(),
            "can_redo" => !m.redo.is_empty(),
            "undo_label" => label("dock.undo.undo", undo_top),
            "redo_label" => label("dock.undo.redo", m.redo.last()),
            "depth" => m.undo.len() as i64,
            "dirty" => m.is_dirty(),
        }
    }
    /// Start a group: every change until `end_group` is one undo step (a handle drag, a paint stroke).
    /// `label` is a text key shown in the Edit menu.
    #[func]
    fn begin_group(&mut self, label: StringName) {
        let Some(m) = self.model.as_mut() else { return };
        let l = label.to_string();
        m.open_group(if l.trim().is_empty() { "dock.undo.edit" } else { l.trim() });
    }
    /// Close the group; with `cancel` true the design returns to its state at `begin_group`.
    #[func]
    fn end_group(&mut self, cancel: bool) {
        let Some(m) = self.model.as_mut() else { return };
        if m.group.is_none() {
            self.state.dragging = false;
            return;
        }
        m.close_group(cancel);
        let was_drag = std::mem::take(&mut self.state.dragging);
        let rev = m.rev;
        if !cancel && was_drag && self.state.coarse_rev == rev && self.state.announced == rev {
            // The drag ended on this revision: its gauges were coarse; ask again at full resolution (L187).
            if let Some((crev, bp)) = self.state.current.clone().filter(|(r, _)| *r == rev) {
                let cap = m.context.cap_t;
                self.state.latency.committed(crev);
                self.submit_job("gauges", crate::worker::Job::FastGauges { rev, blueprint: bp, cap_t: cap, dragging: false });
            }
        }
        self.announce();
        self.base_mut().emit_signal("undo_changed", &[]);
    }

    // ---- Tools, symmetry, selection, clipboard ----

    /// `Bp.TOOL_*`.
    #[func]
    fn set_tool(&mut self, tool: i64) {
        let Some(t) = EditorTool::from_gd(tool) else { return };
        let Some(m) = self.model.as_mut() else { return };
        if m.tool == t { return; }
        m.tool = t;
        let placing = matches!(t, EditorTool::PlacePart | EditorTool::PlaceKit);
        let dropped = !placing && matches!(m.ghost, Some(Ghost::Part { .. } | Ghost::Kit { .. } | Ghost::Paste { .. }));
        if dropped { m.ghost = None; }
        self.emit_tool_changed();
        if dropped { self.base_mut().emit_signal("ghost_changed", &[]); }
    }
    #[func]
    fn tool(&self) -> i64 { self.model.as_ref().map_or(EditorTool::Select.to_gd(), |m| m.tool.to_gd()) }
    /// `symmetry` is `Bp.SYMMETRY_*`; `radial_count` 2..8 applies to radial symmetry only.
    ///
    /// Only half and quarter turns map the 0.5 m cell lattice onto itself, so radial symmetry places 2 copies
    /// (a half turn) for a count of 2 and 4 copies (quarter turns) for any count from 3 to 8; the stored count
    /// is the one used.
    #[func]
    fn set_symmetry(&mut self, symmetry: i64, radial_count: i64) {
        let Some(s) = Symmetry::from_gd(symmetry) else { return };
        let Some(m) = self.model.as_mut() else { return };
        let count = if radial_count <= 2 { 2 } else { 4 };
        if m.symmetry == s && (s != Symmetry::Radial || m.radial_count == count) { return; }
        m.symmetry = s;
        if s == Symmetry::Radial { m.radial_count = count; }
        self.state.parts.invalidate();
        self.emit_tool_changed();
        if m_ghost(self) { self.base_mut().emit_signal("ghost_changed", &[]); }
    }
    /// Returns `{ "symmetry": int, "radial_count": int }`.
    #[func]
    fn symmetry(&self) -> VarDictionary {
        match self.model.as_ref() {
            Some(m) => d! { "symmetry" => m.symmetry.to_gd(), "radial_count" => m.radial_count as i64 },
            None => d! { "symmetry" => Symmetry::Mirror.to_gd(), "radial_count" => 2i64 },
        }
    }
    /// Select placed parts by slot. `additive` adds to or removes from the selection (Shift-click).
    #[func]
    fn select_parts(&mut self, slots: PackedInt32Array, additive: bool) {
        let Some(m) = self.model.as_mut() else { return };
        let n = m.blueprint.parts.len();
        let picked: Vec<PartSlot> = slots.as_slice().iter().filter_map(|&s| u16::try_from(s).ok()).map(PartSlot).filter(|s| s.index() < n).take(n).collect();
        let before = m.selection.clone();
        if additive {
            for s in picked {
                if let Some(i) = m.selection.parts.iter().position(|x| *x == s) { m.selection.parts.remove(i); } else { m.selection.parts.push(s); }
            }
        } else {
            m.selection = Selection { parts: picked, kits: Vec::new(), cells: Vec::new() };
        }
        m.selection.parts.sort_unstable();
        m.selection.parts.dedup();
        if m.selection != before { self.base_mut().emit_signal("selection_changed", &[]); }
    }
    /// Select placed kits by kit slot (selects their parts too).
    #[func]
    fn select_kits(&mut self, kits: PackedInt32Array, additive: bool) {
        let Some(m) = self.model.as_mut() else { return };
        let n = m.blueprint.kits.len();
        let picked: Vec<KitSlot> = kits.as_slice().iter().filter_map(|&s| u16::try_from(s).ok()).map(KitSlot).filter(|k| k.index() < n).take(n).collect();
        let before = m.selection.clone();
        if !additive { m.selection = Selection::default(); }
        for k in picked {
            let parts: Vec<PartSlot> = m.blueprint.kits[k.index()].parts.iter().copied().filter(|s| s.index() < m.blueprint.parts.len()).collect();
            let had = m.selection.kits.contains(&k);
            if additive && had {
                m.selection.kits.retain(|x| *x != k);
                m.selection.parts.retain(|s| !parts.contains(s));
            } else {
                if !had { m.selection.kits.push(k); }
                for s in parts { if !m.selection.parts.contains(&s) { m.selection.parts.push(s); } }
            }
        }
        m.selection.kits.sort_unstable();
        m.selection.parts.sort_unstable();
        if m.selection != before { self.base_mut().emit_signal("selection_changed", &[]); }
    }
    #[func]
    fn select_all(&mut self) {
        let Some(m) = self.model.as_mut() else { return };
        let sel = Selection { parts: (0..m.blueprint.parts.len().min(u16::MAX as usize)).map(|i| PartSlot(i as u16)).collect(), kits: (0..m.blueprint.kits.len().min(u16::MAX as usize)).map(|i| KitSlot(i as u16)).collect(), cells: Vec::new() };
        if m.selection == sel { return; }
        m.selection = sel;
        self.base_mut().emit_signal("selection_changed", &[]);
    }
    #[func]
    fn clear_selection(&mut self) {
        let Some(m) = self.model.as_mut() else { return };
        if m.selection == Selection::default() { return; }
        m.selection = Selection::default();
        self.base_mut().emit_signal("selection_changed", &[]);
    }
    /// Returns `SelectionDict`.
    #[func]
    fn selection(&self) -> VarDictionary {
        let (cells, centre, mass, cost) = self.selection_summary();
        let (parts, kits): (PackedInt32Array, PackedInt32Array) = match self.model.as_ref() {
            Some(m) => (m.selection.parts.iter().map(|s| s.0 as i32).collect(), m.selection.kits.iter().map(|k| k.0 as i32).collect()),
            None => (PackedInt32Array::new(), PackedInt32Array::new()),
        };
        d! { "parts" => parts, "kits" => kits, "cells" => cells as i64, "centre" => centre, "mass_kg" => mass, "cost" => cost }
    }
    /// Remove the selected parts and kits (one undo step). Returns how many parts were removed.
    #[func]
    fn delete_selection(&mut self) -> i64 {
        let Some(m) = self.model.as_ref() else { return 0 };
        let slots = selected_slots(&m.blueprint, &m.selection);
        if slots.is_empty() { return 0; }
        // Descending, so each removal leaves the slots still to go where they were; one `Several` is one
        // copy, one renumbering pass per part and one rehash.
        let edits: Vec<sim_systems::editor::BlueprintEdit> = slots.iter().rev().map(|s| sim_systems::editor::BlueprintEdit::RemovePart(*s)).collect();
        let n = edits.len() as i64;
        let edit = if edits.len() == 1 { edits.into_iter().next().expect("one edit") } else { sim_systems::editor::BlueprintEdit::Several(edits) };
        if !self.commit_edit(&edit, "dock.undo.delete") { return 0; }
        if let Some(m) = self.model.as_mut() { m.selection = Selection::default(); }
        self.base_mut().emit_signal("selection_changed", &[]);
        n
    }
    /// Copy the selection to the editor clipboard. Returns false when nothing is selected.
    #[func]
    fn copy_selection(&mut self) -> bool {
        let cat = crate::services::services().catalogue();
        let Some(m) = self.model.as_mut() else { return false };
        let slots = selected_slots(&m.blueprint, &m.selection);
        let Some(kit) = make_prefab(&m.blueprint, &slots, "clipboard", "", "", &cat) else { return false };
        m.clipboard = Some(Box::new(kit));
        self.state.parts.paste_rot = Default::default();
        true
    }
    /// Start pasting: the clipboard follows the cursor as a ghost until placed or cancelled (`parts.rs`).
    #[func]
    fn paste(&mut self) -> bool {
        let Some(m) = self.model.as_mut() else { return false };
        if m.clipboard.is_none() { return false; }
        let at = match &m.ghost {
            Some(Ghost::Part { cell, .. } | Ghost::Kit { cell, .. }) => *cell,
            Some(Ghost::Paste { offset }) => *offset,
            _ => parts::default_ghost_cell(&m.blueprint),
        };
        m.ghost = Some(Ghost::Paste { offset: at });
        m.tool = EditorTool::PlacePart;
        self.state.parts.invalidate();
        self.emit_tool_changed();
        self.base_mut().emit_signal("ghost_changed", &[]);
        true
    }
    /// Capture the selection as a kit (prefab) named `name`, with version 1 and the author lineage of its
    /// source (L233). Saves it under `user://blueprints/kits/` and returns its `KitDefDict`, or an empty
    /// Dictionary when the selection cannot form a kit.
    #[func]
    fn capture_prefab(&mut self, name: GString) -> VarDictionary {
        use godot::classes::{DirAccess, FileAccess};
        let services = crate::services::services();
        let cat = services.catalogue();
        let author = services.settings.borrow().display_name.clone();
        drop(services);
        let Some(m) = self.model.as_ref() else { return VarDictionary::new() };
        let slots = selected_slots(&m.blueprint, &m.selection);
        let mut name = clean_name(&name.to_string());
        if name.is_empty() { name = default_prefab_name(); }
        let base = kit_id_from_name(&name);
        // A free id: never a stock kit's, never an existing file's.
        let dir = parts::PLAYER_KIT_DIR;
        let mut id = base.clone();
        let mut n = 2;
        while cat.kit(&content::ids::KitId::new(id.clone())).is_some() || FileAccess::file_exists(&GString::from(format!("{dir}/{id}.kit.ron").as_str())) {
            id = format!("{base}_{n}");
            n += 1;
            if n > 999 { return VarDictionary::new(); }
        }
        let Some(kit) = make_prefab(&m.blueprint, &slots, &id, &name, &author, &cat) else { return VarDictionary::new() };
        if !content::validate::validate_kit(&kit, &cat, &content::ruleset::Ruleset::campaign(content::enums::Act::Act4)).is_empty() { return VarDictionary::new(); }
        let _ = DirAccess::make_dir_recursive_absolute(dir);
        let path = format!("{dir}/{id}.kit.ron");
        let Some(mut f) = FileAccess::open(&GString::from(path.as_str()), godot::classes::file_access::ModeFlags::WRITE) else {
            godot_warn!("dry dock: cannot write {path}");
            return VarDictionary::new();
        };
        f.store_string(&GString::from(content::kit::kit_to_file_text(&kit).as_str()));
        f.close();
        self.state.parts.forget_player_kits();
        let summary = crate::content_api::model::kit_summary(&kit, &cat);
        crate::content_api::dicts::kit_def_dict(&kit, &summary, false)
    }
}

/// True when a ghost is up (symmetry changes move its copies).
fn m_ghost(e: &BpEditor) -> bool { e.model.as_ref().is_some_and(|m| m.ghost.is_some()) }

/// A prefab name for an unnamed capture, in the player's language (it becomes player text in the file).
fn default_prefab_name() -> String { godot::classes::TranslationServer::singleton().translate("dock.prefab.default_name").to_string() }

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> DockContext {
        let (ruleset, cap) = mode_rules(EditorMode::Sandbox, None, None);
        DockContext { mode: EditorMode::Sandbox, ruleset, act: content::enums::Act::Act4, cap_t: cap, brief: None, fielded: None, available: None, parts_store: Arc::new(BTreeMap::new()) }
    }
    fn model() -> EditModel { EditModel::new(content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").unwrap(), String::new(), ctx(), 1, true) }
    fn renamed(bp: &Blueprint, name: &str) -> Blueprint { let mut b = bp.clone(); b.header.name = name.into(); b }

    #[test]
    fn undo_redo_and_revisions_only_grow() {
        let mut m = model();
        let a = renamed(&m.blueprint, "A");
        m.apply(a.clone(), "dock.undo.rename");
        assert_eq!(m.rev, 2);
        assert!(m.is_dirty());
        m.apply(a.clone(), "dock.undo.rename");
        assert_eq!(m.rev, 2, "an equal value is no step");
        let b = renamed(&a, "B");
        m.apply(b.clone(), "dock.undo.rename");
        assert!(m.undo_step());
        assert_eq!(m.blueprint.header.name, "A");
        assert_eq!(m.rev, 4);
        assert!(m.redo_step());
        assert_eq!(m.blueprint.header.name, "B");
        assert_eq!(m.rev, 5);
        assert!(m.undo_step() && m.undo_step());
        assert!(!m.undo_step());
        // A new change clears redo.
        m.apply(renamed(&m.blueprint, "C"), "dock.undo.rename");
        assert!(m.redo.is_empty());
    }

    #[test]
    fn a_group_is_one_step_and_cancel_restores() {
        let mut m = model();
        let start = m.blueprint.clone();
        m.open_group("dock.undo.handle");
        for i in 0..5 { m.apply(renamed(&m.blueprint, &format!("n{i}")), "dock.undo.rename"); }
        assert_eq!(m.rev, 6, "each change in a group is a revision for the views");
        m.close_group(false);
        assert_eq!(m.undo.len(), 1);
        assert_eq!(m.undo[0].1.key.as_str(), "dock.undo.handle");
        assert!(m.undo_step());
        assert_eq!(m.blueprint, start);
        let rev = m.rev;
        m.open_group("dock.undo.handle");
        m.apply(renamed(&m.blueprint, "x"), "dock.undo.rename");
        assert!(m.close_group(true));
        assert_eq!(m.blueprint, start);
        assert!(m.rev > rev + 1, "the cancel is a new revision too");
        assert_eq!(m.undo.len(), 0);
    }

    #[test]
    fn undo_depth_is_bounded() {
        let mut m = model();
        for i in 0..(UNDO_DEPTH + 10) { m.apply(renamed(&m.blueprint, &format!("n{i}")), "dock.undo.rename"); }
        assert_eq!(m.undo.len(), UNDO_DEPTH);
        assert_eq!(m.undo[0].0.header.name, "n9");
    }

    #[test]
    fn latency_switches_the_split_strip_on_and_off() {
        let mut l = GaugeLatency::default();
        for _ in 0..49 { l.record(150.0); }
        assert!(!l.split_strip, "needs 50 edits");
        l.record(150.0);
        assert!(l.split_strip);
        for _ in 0..49 { l.record(50.0); }
        assert!(l.split_strip);
        l.record(80.0);
        for _ in 0..49 { l.record(50.0); }
        assert!(l.split_strip, "the run of 50 under 70 ms restarted");
        l.record(50.0);
        assert!(!l.split_strip);
        assert!(l.p95() <= 80.0);
    }

    #[test]
    fn mode_rules_follow_the_spec() {
        let (r, cap) = mode_rules(EditorMode::Campaign, Some(content::enums::Act::Act2), None);
        assert_eq!(cap, 400.0);
        assert_eq!(max_layer(&r), content::enums::ControlLayer::L4Script);
        let std = lobby_rulesets().iter().find(|(k, _)| k == "standard").map(|(_, r)| r.clone()).unwrap();
        let (r, cap) = mode_rules(EditorMode::Lobby, None, Some(&std));
        assert_eq!(cap, 400.0);
        assert_eq!(max_layer(&r), content::enums::ControlLayer::L2Rules);
        assert_eq!(max_length_m(EditorMode::Lobby, &r), 90.0);
        let (r, _) = mode_rules(EditorMode::EnemyClass, None, None);
        assert_eq!(max_length_m(EditorMode::EnemyClass, &r), 150.0);
    }

    #[test]
    fn prefab_keeps_lineage_and_relative_offsets() {
        let cat = content::catalogue::catalogue();
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let k = bp.kits.iter().position(|k| k.parts.len() > 1).expect("a starter kit with several parts");
        let slots = bp.kits[k].parts.clone();
        let kit = make_prefab(&bp, &slots, "player.test", "Test mount", "Someone", cat).unwrap();
        assert_eq!(kit.version, 1);
        assert_eq!(kit.parts.len(), slots.len());
        let src = bp.embedded_kits.get(&bp.kits[k].kit).or_else(|| cat.kit(&bp.kits[k].kit)).unwrap();
        assert_eq!(kit.lineage.last().unwrap().hash, content::kit::kit_hash(src), "a whole kit's capture descends from that kit");
        assert!(kit.parts.iter().any(|p| p.offset.x == 0) && kit.parts.iter().any(|p| p.offset.y == 0) && kit.parts.iter().any(|p| p.offset.z == 0));
        assert!(content::validate::validate_kit(&kit, cat, &content::ruleset::Ruleset::campaign(content::enums::Act::Act4)).is_empty());
        // A loose selection descends from the design.
        let loose: Vec<PartSlot> = (0..bp.parts.len()).filter(|i| bp.parts[*i].kit.is_none()).take(2).map(|i| PartSlot(i as u16)).collect();
        if !loose.is_empty() {
            let kit = make_prefab(&bp, &loose, "player.loose", "Loose", "", cat).unwrap();
            assert_eq!(kit.lineage.last().unwrap().hash, content::blueprint::io::compute_hash(&bp));
            assert!(kit.author.is_none());
        }
        assert!(make_prefab(&bp, &[], "player.none", "None", "", cat).is_none());
    }

    #[test]
    fn the_mirror_plane_ignores_one_sided_superstructure() {
        let bp = content::campaign::load::stock_blueprint("starters/fac22.bp.ron").unwrap();
        let s = mirror_sum(&bp).unwrap();
        let g = content::cell::CellGrid::from_rows(&bp.cells);
        let structure = {
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for r in &bp.cells.0 { if !r.runs.is_empty() { lo = lo.min(r.z as i32); hi = hi.max(r.z as i32); } }
            lo + hi
        };
        assert_eq!(s, structure, "the shell's centreline is the structure's mid-plane");
        // A bridge wing one cell past the starboard side.
        let mut grid = content::cell::CellGrid::new([g.size[0], g.size[1], g.size[2] + 1]);
        for (i, c) in g.cells.iter().enumerate() { grid.set(g.pos(content::ids::CellIndex(i as u32)), *c); }
        let top = g.size[1] as i16 - 1;
        grid.set(CellPos::new(g.size[0] as i16 / 2, top, g.size[2] as i16), content::cell::Cell { material: content::cell::Material::Steel, armour: Default::default(), paint: 2, shape: content::cell::CellShape::CUBE });
        let mut wing = bp.clone();
        wing.cells = grid.to_rows();
        assert_eq!(mirror_sum(&wing), Some(s));
        let mut baked = wing.clone();
        baked.hull.source = content::hull::HullSource::Freeform;
        assert_eq!(mirror_sum(&baked), Some(s), "a baked hull takes the plane from below the waterline");
    }

    #[test]
    fn names_and_ids_are_clean() {
        assert_eq!(kit_id_from_name("  Twin 76 mm / Mk.2 "), "player.twin_76_mm_mk_2");
        assert_eq!(kit_id_from_name("!!!"), "player.kit");
        assert_eq!(clean_name("  a\u{7}b  "), "ab");
        assert_eq!(clean_name(&"x".repeat(200)).chars().count(), MAX_NAME_CHARS);
        assert_eq!(stock_key("stock:starters/fac22.bp.ron").as_deref(), Some("starters/fac22.bp.ron"));
        assert_eq!(stock_key("blueprints/starters/fac22.bp.ron").as_deref(), Some("starters/fac22.bp.ron"));
        assert!(stock_key("user://blueprints/a.bp.ron").is_none());
    }

    #[test]
    fn every_dock_key_has_an_english_row() {
        let lk = crate::text::tests::CsvLookup::load();
        for key in text_keys() {
            assert!(lk.0.contains_key(key), "game/i18n/dock.csv lacks {key}");
        }
    }
}
