//! Unit `gd-app`. `BpApp` (autoload `App`): process-level services.
//! DESIGN.md L1270 (startup log of rendering method, driver, adapter, API version and every fallback; values
//! go into crash and benchmark reports; safe mode notice), L1330-1347 (crash reports: Sentry, opt-in, Rust
//! panics through the `sentry` crate after consent, one function sets each tag in both SDKs; a debug command
//! panics once on the main thread and once on a worker), L1003 (random 64-bit client id per install),
//! L80 (local playtest timing log: never sent; Settings > Playtest > Copy timing log), L881 (local
//! operation log: loop times, income, spend, ghost results), L2209 (a copyable playtest report written at
//! session end), L1039 (diagnostics bundle with the connection log), L1359-1360 (user data paths).
//! The Sentry Godot SDK is driven from GDScript (`res://boot/consent.gd`); this class owns the Rust side,
//! the `sentry` crate line of L1338: panics only, no minidumps, no release health. Events carry no host
//! name, user, or folder of the player: `before_send` strips `server_name`, `user` and the folders of
//! debug-image paths (symbolication uses the debug id, not the path).
//!
//! Files: `user://identity.ron` (client id), [`TIMING_LOG`] and [`OPERATION_LOG`] (one row per core loop,
//! appended when the loop closes), [`SESSION_REPORT`] (the playtest report, rewritten when a loop closes
//! and at exit), [`RUNNING_FLAG`] (present while the game runs; a crash leaves it behind),
//! [`CONNECTION_LOG`] (written by the lobby, read here for diagnostics). Godot's own log, with every Rust
//! panic and engine crash backtrace, is `user://logs/godot.log`; Godot renames it with a date at the next
//! start.
//!
//! # The loop logs (L80, L881)
//! No screen marks loop steps: `process` reads the `Campaign` autoload's phase each frame and files the
//! frame's active time under one step ([`loop_step`]). A loop opens when a briefing opens
//! ([`starts_new_loop`]) and closes when the next loop opens, when the campaign closes, or at exit. The
//! clock leaves out sortie pauses over 2 minutes and suspended stretches ([`ActiveClock`]). Splash time is
//! dry-dock time while a splash session is at sea. Income and spend are the rises and falls of the Salvage
//! ledger while the loop is open. First attempt means no earlier loop on this install sortied on the same
//! operation of the same campaign (theatre seed).
use godot::classes::file_access::ModeFlags;
use godot::classes::{DirAccess, Engine, FileAccess, Os, Time};
use godot::obj::Singleton;
use godot::prelude::*;
use std::collections::BTreeMap;

/// The local connection log the lobby appends to (L1039).
pub const CONNECTION_LOG: &str = "user://logs/connection.log";
pub const TIMING_LOG: &str = "user://logs/timing_log.txt";
pub const OPERATION_LOG: &str = "user://logs/operation_log.txt";
/// The playtest report as one copyable text (L2209), rewritten at session end.
pub const SESSION_REPORT: &str = "user://logs/playtest_report.txt";
/// The tutorial's step log (`tutorial.rs`), folded into the copied timing log and the report.
pub const TUTORIAL_LOG: &str = "user://logs/tutorial_log.txt";
/// Holds the process id while the game runs; removed at a clean exit.
pub const RUNNING_FLAG: &str = "user://logs/running.flag";
/// Godot's own log of this run.
pub const GODOT_LOG: &str = "user://logs/godot.log";
pub const IDENTITY_FILE: &str = "user://identity.ron";
/// The user data folders `user_dirs` creates (L1359-1360).
pub const USER_DIRS: [(&str, &str); 7] = [
    ("blueprints", "user://blueprints"), ("kits", "user://kits"), ("saves", "user://saves"), ("replays", "user://replays"),
    ("logs", "user://logs"), ("bench", "user://bench"), ("captures", "user://captures"),
];
/// A pause of the core loop longer than this is left out of the timing rows (L80).
pub const LONG_PAUSE_S: f64 = 120.0;
/// A frame gap longer than this is time the app was suspended or stalled; it is left out too.
const SUSPEND_GAP_S: f64 = 5.0;
/// Exit budget (doc of `exit_tree`).
const EXIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);
const CRASH_FLUSH_MAX: std::time::Duration = std::time::Duration::from_secs(2);

// ---------------------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------------------

/// Parsed user arguments (after `--` on the command line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandLine { pub smoke: bool, pub smoke_sortie: bool, pub benchmark: bool, pub replay: String, pub join: String, pub scripted: String, pub capture: String, pub safe_mode: bool }

/// `--smoke`, `--smoke-sortie`, `--benchmark`, `--safe-mode`, and `--replay`, `--join`, `--scripted`, `--capture` with a value
/// given as `--name=value` or `--name value`. Unknown arguments are ignored.
pub fn parse_command_line(args: &[String]) -> CommandLine {
    let mut c = CommandLine::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].trim();
        let (name, inline) = match arg.split_once('=') { Some((n, v)) => (n, Some(v.to_string())), None => (arg, None) };
        let takes_value = matches!(name, "--replay" | "--join" | "--scripted" | "--capture");
        let value = if takes_value {
            match inline {
                Some(v) => v,
                None if i + 1 < args.len() && !args[i + 1].starts_with("--") => { i += 1; args[i].clone() }
                None => String::new(),
            }
        } else { String::new() };
        match name {
            "--smoke" => c.smoke = true,
            "--smoke-sortie" => c.smoke_sortie = true,
            "--benchmark" => c.benchmark = true,
            "--safe-mode" | "--safe_mode" => c.safe_mode = true,
            "--replay" => c.replay = value,
            "--join" => c.join = value,
            "--scripted" => c.scripted = value,
            "--capture" => c.capture = value,
            _ => {}
        }
        i += 1;
    }
    c
}

/// This process's user arguments (after `--`), parsed. Main thread only (it reads `Os`).
pub(crate) fn user_command_line() -> CommandLine {
    let args: Vec<String> = godot::classes::Os::singleton().get_cmdline_user_args().as_slice().iter().map(|s| s.to_string()).collect();
    parse_command_line(&args)
}

/// What the renderer started as (L1270).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartupReport { pub rendering_method: String, pub driver: String, pub adapter: String, pub api_version: String, pub fallbacks: Vec<String> }
impl StartupReport {
    /// Safe mode is the Compatibility renderer (L1270).
    pub fn safe_mode(&self) -> bool { self.rendering_method.trim() == "gl_compatibility" }
    pub fn log_line(&self) -> String {
        let fallbacks = if self.fallbacks.is_empty() { "none".to_string() } else { self.fallbacks.join(", ") };
        format!("renderer: method={} driver={} adapter=\"{}\" api={} fallbacks={}{}", self.rendering_method, self.driver, self.adapter, self.api_version, fallbacks, if self.safe_mode() { " (safe mode)" } else { "" })
    }
}

/// The client id file body and its parse. The id is never 0 or `u64::MAX` (L1003).
pub fn identity_ron(id: u64) -> String { format!("(client_id: {id})\n") }
pub fn parse_identity(text: &str) -> Option<u64> {
    let rest = &text[text.find("client_id")? + "client_id".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().ok().filter(|v| *v != 0 && *v != u64::MAX)
}

/// The install's client id, read from [`IDENTITY_FILE`] or created there on first use (L1003). Every link,
/// `Lobby` and `Session` alike, takes its id from here, so two installs never share one: a missing file used
/// to read as id 1 everywhere, and the second copy of the game was refused as already in the lobby.
pub fn install_client_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    /// The id of this run when the file cannot be written, so it at least stays the same until quit.
    static SESSION_ID: AtomicU64 = AtomicU64::new(0);
    if FileAccess::file_exists(IDENTITY_FILE) {
        if let Some(id) = parse_identity(&FileAccess::get_file_as_string(IDENTITY_FILE).to_string()) { return id; }
        godot_warn!("app: {IDENTITY_FILE} is damaged; a new client id replaces it");
    }
    let kept = SESSION_ID.load(Ordering::Relaxed);
    let id = if kept != 0 { kept } else { net::join::ClientIdentity::generate("").client_id };
    SESSION_ID.store(id, Ordering::Relaxed);
    match FileAccess::open(IDENTITY_FILE, ModeFlags::WRITE) {
        Some(mut f) => { f.store_string(&identity_ron(id)); f.close(); }
        None => { godot_warn!("app: could not write {IDENTITY_FILE}; the client id lasts this session only"); }
    }
    id
}

/// The clock the timing log measures with: wall time minus long pauses and suspended stretches (L80).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActiveClock { pub active_s: f64, pause_s: f64, paused: bool }
impl ActiveClock {
    /// Advance by one frame. Short pauses count as play; a pause past [`LONG_PAUSE_S`] is left out entirely,
    /// as is a single frame gap over 5 s (the app was suspended).
    pub fn advance(&mut self, delta_s: f64, paused: bool) {
        let d = if delta_s.is_finite() { delta_s.max(0.0) } else { 0.0 };
        if d > SUSPEND_GAP_S { self.end_pause(); return; }
        if paused {
            self.paused = true;
            self.pause_s += d;
        } else {
            self.end_pause();
            self.active_s += d;
        }
    }
    fn end_pause(&mut self) {
        if self.paused && self.pause_s <= LONG_PAUSE_S { self.active_s += self.pause_s; }
        self.paused = false;
        self.pause_s = 0.0;
    }
    /// The time a mark records: pause time so far counts while it is still short.
    pub fn now(&self) -> f64 { self.active_s + if self.paused && self.pause_s <= LONG_PAUSE_S { self.pause_s } else { 0.0 } }
}

/// The steps of the core loop (L62-73) the timing log measures, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopStep { Briefing, Refit, Splash, Sortie, Recover, Chart }
impl LoopStep {
    pub const ALL: [LoopStep; 6] = [LoopStep::Briefing, LoopStep::Refit, LoopStep::Splash, LoopStep::Sortie, LoopStep::Recover, LoopStep::Chart];
    pub fn name(self) -> &'static str {
        match self { LoopStep::Briefing => "briefing", LoopStep::Refit => "refit", LoopStep::Splash => "splash", LoopStep::Sortie => "sortie", LoopStep::Recover => "recover", LoopStep::Chart => "chart" }
    }
    fn index(self) -> usize { LoopStep::ALL.iter().position(|s| *s == self).unwrap_or(0) }
}

/// The step a frame belongs to, from the campaign phase and whether a splash test is at sea. `None` with
/// no campaign loaded or after an Ironman loss.
pub fn loop_step(phase: crate::enums::CampaignPhase, splash_live: bool) -> Option<LoopStep> {
    use crate::enums::CampaignPhase as P;
    match phase {
        P::Briefing => Some(LoopStep::Briefing),
        P::DryDock if splash_live => Some(LoopStep::Splash),
        P::DryDock => Some(LoopStep::Refit),
        P::Sortie => Some(LoopStep::Sortie),
        P::AfterAction => Some(LoopStep::Recover),
        P::Chart | P::Advancing => Some(LoopStep::Chart),
        P::None | P::Over => None,
    }
}

/// One core loop as the timing and operation logs see it, from opening a briefing to opening the next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenLoop {
    /// Local date and time the briefing opened.
    pub started: String,
    /// Version and build id, so a row can be matched to a release.
    pub build: String,
    /// Theatre seed in hex: tells one campaign's operations from another's.
    pub campaign: String,
    pub operation: String,
    /// The mission template (`convoy_raid`), or `story`, `ace_duel`, `capstone`, `tutorial`.
    pub template: String,
    /// Difficulty preset id, or `custom`.
    pub difficulty: String,
    /// Active seconds per [`LoopStep`].
    pub step_s: [f64; 6],
    pub sortied: bool,
    /// Whether this loop's sortie was the first on its operation; `None` before a result.
    pub first_attempt: Option<bool>,
    /// Outcome of this loop's sortie (`victory`, `defeat`, `withdrawn`, `aborted`, `draw`).
    pub outcome: Option<String>,
    pub salvage: Option<u32>,
    pub income: u64,
    pub spend: u64,
    pub ghosts_met: u32,
    pub ghosts_total: u32,
}
impl OpenLoop {
    /// File `dt` active seconds under `step`. A negative `dt` (the clock took back a pause that grew past
    /// 2 minutes) comes off the same step; no step goes below zero.
    pub fn add_time(&mut self, step: LoopStep, dt: f64) {
        if !dt.is_finite() { return; }
        let slot = &mut self.step_s[step.index()];
        *slot = (*slot + dt).max(0.0);
    }
    /// The Salvage ledger now: a rise is income, a fall is spend.
    pub fn salvage_now(&mut self, salvage: u32) {
        if let Some(prev) = self.salvage {
            if salvage > prev { self.income += u64::from(salvage - prev); } else { self.spend += u64::from(prev - salvage); }
        }
        self.salvage = Some(salvage);
    }
    pub fn loop_s(&self) -> f64 { self.step_s.iter().sum() }
    fn outcome_text(&self) -> &str { self.outcome.as_deref().unwrap_or("none") }
    /// The timing log row (L80): briefing-open time, seconds per step, sortie length, template, difficulty
    /// preset and first-attempt outcome. Tab-separated `key=value`; `incomplete` when the loop never
    /// reached the next briefing.
    pub fn timing_row(&self, complete: bool) -> String {
        let mut row = format!("{}\tbuild={}\tcampaign={}\toperation={}\ttemplate={}\tdifficulty={}", self.started, self.build, self.campaign, self.operation, self.template, self.difficulty);
        for step in LoopStep::ALL { row.push_str(&format!("\t{}={:.0}", step.name(), self.step_s[step.index()])); }
        let first = match self.first_attempt { Some(true) => "yes", Some(false) => "no", None => "none" };
        row.push_str(&format!("\tloop={:.0}\tfirst_attempt={first}\toutcome={}", self.loop_s(), self.outcome_text()));
        if !complete { row.push_str("\tincomplete"); }
        row.push('\n');
        row
    }
    /// The operation log row (L881): loop times, income, spend and ghost results.
    pub fn operation_row(&self, complete: bool) -> String {
        let mut row = format!(
            "{}\tbuild={}\tcampaign={}\toperation={}\tloop={:.0}\tsortie={:.0}\tincome={}\tspend={}\tghosts_met={}/{}\toutcome={}",
            self.started, self.build, self.campaign, self.operation, self.loop_s(), self.step_s[LoopStep::Sortie.index()], self.income, self.spend, self.ghosts_met, self.ghosts_total, self.outcome_text(),
        );
        if !complete { row.push_str("\tincomplete"); }
        row.push('\n');
        row
    }
    /// The key [`attempted_keys`] reads back.
    pub fn attempt_key(&self) -> String { format!("{}/{}", self.campaign, self.operation) }
}

/// Opening the briefing of `operation` starts a new loop unless the open loop is the same operation and has
/// not sortied yet (the player looked at the chart table or the dry dock and came back).
pub fn starts_new_loop(open: Option<&OpenLoop>, operation: &str) -> bool {
    match open { None => true, Some(l) => l.sortied || l.operation != operation }
}

/// `campaign/operation` of every operation-log row that sortied, for first-attempt checks.
pub fn attempted_keys(operation_log: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for line in operation_log.lines() {
        let field = |k: &str| line.split('\t').find_map(|f| f.strip_prefix(k)).map(str::to_string);
        let (Some(c), Some(o), Some(outcome)) = (field("campaign="), field("operation="), field("outcome=")) else { continue };
        if outcome != "none" { out.insert(format!("{c}/{o}")); }
    }
    out
}

/// The template column for an operation kind.
pub fn template_name(kind: &content::campaign::operation::OperationKind) -> String {
    use content::campaign::operation::OperationKind as K;
    match kind {
        K::Template(m) => snake(&format!("{m:?}")),
        K::Story => "story".into(),
        K::AceDuel(_) => "ace_duel".into(),
        K::Capstone => "capstone".into(),
        K::Tutorial => "tutorial".into(),
    }
}

/// `ConvoyRaid` to `convoy_raid`.
pub fn snake(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() { if i > 0 { out.push('_'); } out.push(c.to_ascii_lowercase()); } else { out.push(c); }
    }
    out
}

/// A log file's text for a copy: the build line, then the file, or `""` when the file holds no rows.
pub fn copy_text(build_line: &str, title: &str, body: &str) -> String {
    if body.trim().is_empty() { return String::new(); }
    let mut t = format!("{build_line}\n{title}\n{body}");
    if !t.ends_with('\n') { t.push('\n'); }
    t
}

/// Hide the player's folders and names in text bound for a bug report: the home and user-data folders
/// become `<home>` and `<user>`, and quoted `name` fields of RON lines become `"-"`.
pub fn scrub(text: &str, home: &str, user_dir: &str) -> String {
    let mut s = text.to_string();
    for (needle, with) in [(user_dir, "<user>"), (home, "<home>")] {
        if needle.len() > 2 { s = s.replace(needle, with); s = s.replace(&needle.replace('/', "\\"), with); }
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find("name:") {
        out.push_str(&rest[..i + 5]);
        let after = &rest[i + 5..];
        let trimmed = after.trim_start();
        if let Some(body) = trimmed.strip_prefix('"') {
            match body.find('"') {
                Some(end) => { out.push_str(" \"-\""); rest = &body[end + 1..]; }
                None => { out.push_str(" \"-\""); rest = ""; }
            }
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------------------------
// The autoload
// ---------------------------------------------------------------------------------------------------------

/// Process-level services. Autoload `App`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpApp {
    pub(crate) crash_reports_on: bool,
    pub(crate) startup: Option<StartupReport>,
    pub(crate) sentry_guard: Option<sentry::ClientInitGuard>,
    pub(crate) tags: BTreeMap<String, String>,
    pub(crate) clock: ActiveClock,
    /// The core loop being timed (L80, L881).
    pub(crate) open_loop: Option<OpenLoop>,
    /// Campaign phase seen last frame.
    pub(crate) last_phase: crate::enums::CampaignPhase,
    /// `campaign/operation` keys that have sortied (read from the operation log on first use).
    pub(crate) attempted: Option<std::collections::BTreeSet<String>>,
    pub(crate) campaign_node: Option<Gd<crate::campaign::BpCampaign>>,
    /// The previous run left [`RUNNING_FLAG`] behind: it crashed or was killed.
    pub(crate) previous_run_crashed: bool,
    pub(crate) panicked_on_main: bool,
    pub(crate) panicked_on_worker: bool,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpApp {
    fn init(base: Base<Node>) -> Self {
        Self {
            crash_reports_on: false, startup: None, sentry_guard: None, tags: BTreeMap::new(), clock: ActiveClock::default(), open_loop: None,
            last_phase: crate::enums::CampaignPhase::None, attempted: None, campaign_node: None, previous_run_crashed: false,
            panicked_on_main: false, panicked_on_worker: false, base,
        }
    }
    /// Note a crash of the previous run, then mark this one as running.
    fn ready(&mut self) {
        // The editor and the headless tools load the autoload too; only a game run keeps the flag.
        if Engine::singleton().is_editor_hint() { return; }
        self.previous_run_crashed = check_running_flag();
        if self.previous_run_crashed { godot_warn!("app: the previous run did not close normally; its log is in the logs folder, named godot<date>.log"); }
    }
    /// Advance the timing clock (the sortie pause flag comes from this frame's snapshot) and file the
    /// frame's active time under the core-loop step the campaign is in.
    fn process(&mut self, delta: f64) {
        let paused = crate::services::services().feed.borrow().snapshot.as_ref().is_some_and(|s| s.pacing.paused);
        let before = self.clock.now();
        self.clock.advance(delta, paused);
        let dt = self.clock.now() - before;
        self.watch_loop(dt);
    }
    /// Unit `gd-app`: on exit, stop the session, the worker and the meshing thread, flush crash reports for
    /// up to 2 s and write pending settings. Must not block longer than 3 s in total.
    ///
    /// The meshing thread belongs to `BpWorldView`'s hull renderer, which stops it when the view leaves the
    /// tree (before the autoloads, which are the root's first children and exit last).
    fn exit_tree(&mut self) {
        let start = std::time::Instant::now();
        // Settings first: a fast local write that must not be lost to a slow join below.
        crate::settings::flush();
        // The loop in progress is written as incomplete, then the session's report (L2209).
        self.close_loop(false);
        if !Engine::singleton().is_editor_hint() {
            self.write_session_report();
            clear_running_flag();
        }
        let services = crate::services::services();
        let connection = services.connection.borrow_mut().take();
        if let Some(mut c) = connection {
            c.link.close();
            // `HostHandle::shutdown` blocks at most 2 s.
            if let Some(host) = c.host.as_mut() { host.shutdown(); }
        }
        let worker = services.worker.borrow_mut().take();
        if let Some(mut w) = worker { w.shutdown(); }
        // Whatever is left of the 3 s, at most 2 s, goes to sending queued crash reports.
        let left = EXIT_BUDGET.saturating_sub(start.elapsed()).min(CRASH_FLUSH_MAX);
        if let Some(guard) = self.sentry_guard.take() {
            if let Some(client) = sentry::Hub::current().client() { client.close(Some(left)); }
            drop(guard);
        }
    }
}

impl BpApp {
    /// Follow the campaign phase: open, time and close core loops. Reads the `Campaign` autoload's fields;
    /// a file is written only when a loop closes.
    fn watch_loop(&mut self, dt: f64) {
        use crate::enums::{CampaignPhase, SessionKind, SessionState};
        let services = crate::services::services();
        if !services.settings.borrow().bool("playtest.timing_log") {
            self.open_loop = None;
            return;
        }
        if self.campaign_node.as_ref().is_none_or(|n| !n.is_instance_valid()) {
            self.campaign_node = self.base().get_node_or_null("/root/Campaign").and_then(|n| n.try_cast::<crate::campaign::BpCampaign>().ok());
        }
        let Some(node) = self.campaign_node.clone() else { return };
        let splash_live = { let feed = services.feed.borrow(); feed.kind == SessionKind::Splash && feed.state == SessionState::InSortie };
        drop(services);
        let campaign = node.bind();
        let phase = campaign.phase;
        let entered = phase != self.last_phase;
        self.last_phase = phase;
        let state = campaign.state();
        if entered && phase == CampaignPhase::Briefing {
            let op = campaign.run.briefed.as_ref().map(|o| o.as_str().to_string()).unwrap_or_default();
            if starts_new_loop(self.open_loop.as_ref(), &op) {
                let template = campaign_briefing_kind(&campaign, &op).map(|k| template_name(&k)).unwrap_or_else(|| "unknown".into());
                let difficulty = state.map(|s| preset_name(&s.difficulty)).unwrap_or_default();
                let fresh = OpenLoop {
                    started: Time::singleton().get_datetime_string_from_system().to_string(),
                    build: build_text(),
                    campaign: state.map(|s| format!("{:x}", s.seed)).unwrap_or_default(),
                    operation: op,
                    template,
                    difficulty,
                    salvage: state.map(|s| s.resources.salvage),
                    ..Default::default()
                };
                drop(campaign);
                self.close_loop(true);
                self.open_loop = Some(fresh);
                return;
            }
        }
        let salvage = state.map(|s| s.resources.salvage);
        let result = if entered && phase == CampaignPhase::AfterAction {
            campaign.after_action.as_ref().map(|a| (snake(&format!("{:?}", a.outcome)), a.ghosts.iter().filter(|g| g.1).count() as u32, a.ghosts.len() as u32))
        } else { None };
        drop(campaign);
        if phase == CampaignPhase::None {
            // The campaign closed (back to the main menu): the loop ends there.
            self.close_loop(false);
            return;
        }
        if self.attempted.is_none() && result.is_some() {
            let text = if FileAccess::file_exists(OPERATION_LOG) { FileAccess::get_file_as_string(OPERATION_LOG).to_string() } else { String::new() };
            self.attempted = Some(attempted_keys(&text));
        }
        let Some(open) = self.open_loop.as_mut() else { return };
        if let Some(step) = loop_step(phase, splash_live) { open.add_time(step, dt); }
        if phase == CampaignPhase::Sortie { open.sortied = true; }
        if let Some(s) = salvage { open.salvage_now(s); }
        if let Some((outcome, met, total)) = result {
            if open.outcome.is_none() {
                let key = open.attempt_key();
                let attempted = self.attempted.get_or_insert_with(Default::default);
                open.first_attempt = Some(!attempted.contains(&key));
                attempted.insert(key);
                open.outcome = Some(outcome);
                open.ghosts_met = met;
                open.ghosts_total = total;
                open.sortied = true;
            }
        }
    }

    /// Append the open loop's rows to both logs and refresh the session report.
    fn close_loop(&mut self, complete: bool) {
        let Some(open) = self.open_loop.take() else { return };
        if open.loop_s() < 1.0 && !open.sortied { return; }
        // A loop that never sailed did not run from one briefing to the next.
        let complete = complete && open.sortied;
        append_file(TIMING_LOG, &open.timing_row(complete));
        append_file(OPERATION_LOG, &open.operation_row(complete));
        self.write_session_report();
    }

    /// The playtest report (L2209): build, timing log, operation log and tutorial log in one text.
    fn session_report(&self) -> String {
        let mut t = format!("{}\nplaytest report, written {}\n\n", build_line(), Time::singleton().get_datetime_string_from_system());
        for (title, path) in [("timing log (seconds per step)", TIMING_LOG), ("operation log", OPERATION_LOG), ("tutorial log", TUTORIAL_LOG)] {
            let body = read_text(path);
            t.push_str(&format!("{title}:\n{}\n", if body.trim().is_empty() { "empty\n".to_string() } else { body }));
        }
        if let Some(open) = &self.open_loop { t.push_str(&format!("loop in progress:\n{}", open.timing_row(false))); }
        t
    }
    fn write_session_report(&self) {
        // A run that played no loop and no tutorial has nothing to report.
        if self.open_loop.is_none() && ![TIMING_LOG, OPERATION_LOG, TUTORIAL_LOG].iter().any(|p| FileAccess::file_exists(*p)) { return; }
        let text = self.session_report();
        ensure_dir("user://logs");
        match FileAccess::open(SESSION_REPORT, ModeFlags::WRITE) {
            Some(mut f) => { f.store_string(&text); f.close(); }
            None => { godot_warn!("app: could not write {SESSION_REPORT}"); }
        }
    }
    fn set_tag_rust(&mut self, key: &str, value: &str) {
        self.tags.insert(key.to_string(), value.to_string());
        if self.sentry_guard.is_some() { sentry::configure_scope(|scope| scope.set_tag(key, value)); }
    }
}

fn ensure_dir(path: &str) -> bool { DirAccess::dir_exists_absolute(path) || DirAccess::make_dir_recursive_absolute(path) == godot::global::Error::OK }

fn read_text(path: &str) -> String { if FileAccess::file_exists(path) { FileAccess::get_file_as_string(path).to_string() } else { String::new() } }

fn append_file(path: &str, text: &str) {
    ensure_dir("user://logs");
    let file = if FileAccess::file_exists(path) { FileAccess::open(path, ModeFlags::READ_WRITE) } else { FileAccess::open(path, ModeFlags::WRITE) };
    match file {
        Some(mut f) => { f.seek_end(); f.store_string(text); f.close(); }
        None => { godot_warn!("app: could not write {path}"); }
    }
}

/// `0.1.0 (build 3f2a9c1)`.
fn build_text() -> String { format!("{} (build {})", env!("CARGO_PKG_VERSION"), option_env!("BP_BUILD_ID").unwrap_or("local")) }
/// The first line of every copied log and report.
fn build_line() -> String { format!("Black Pennant {}, {}", build_text(), Os::singleton().get_name()) }

/// The operation kind of an offered or briefed operation.
fn campaign_briefing_kind(campaign: &crate::campaign::BpCampaign, op: &str) -> Option<content::campaign::operation::OperationKind> {
    use crate::campaign::Backend;
    let id = content::ids::OperationId::new(op.to_string());
    match &campaign.backend {
        Some(Backend::Local(t)) => t.briefing(&id).map(|b| b.kind),
        Some(Backend::Remote { briefings, .. }) => briefings.iter().find(|b| b.operation == id).map(|b| b.kind.clone()),
        None => None,
    }
}

/// The difficulty preset whose sliders match, else `custom`.
fn preset_name(s: &content::campaign::DifficultySliders) -> String {
    let data = crate::services::services().campaign();
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
    data.difficulty.iter().find(|p| {
        let q = &p.sliders;
        close(q.ai_accuracy, s.ai_accuracy) && close(q.ai_reaction_s, s.ai_reaction_s) && close(q.adaptation_cadence, s.adaptation_cadence) && q.hull_count == s.hull_count && close(q.economy, s.economy) && q.clock_days == s.clock_days
    }).map_or_else(|| "custom".to_string(), |p| p.id.as_str().to_string())
}

/// True when [`RUNNING_FLAG`] names a process that is no longer running (the last run crashed or was
/// killed). Then writes this process's id into it.
fn check_running_flag() -> bool {
    let os = Os::singleton();
    let mine = os.get_process_id();
    let crashed = match read_text(RUNNING_FLAG).trim().parse::<i32>() {
        Ok(pid) if pid > 0 && pid != mine => !os.is_process_running(pid),
        Ok(_) => false,
        Err(_) => FileAccess::file_exists(RUNNING_FLAG),
    };
    ensure_dir("user://logs");
    match FileAccess::open(RUNNING_FLAG, ModeFlags::WRITE) {
        Some(mut f) => { f.store_string(&mine.to_string()); f.close(); }
        None => { godot_warn!("app: could not write {RUNNING_FLAG}"); }
    }
    crashed
}

/// Remove [`RUNNING_FLAG`] at a clean exit, unless another copy of the game wrote it since.
fn clear_running_flag() {
    let mine = Os::singleton().get_process_id();
    if read_text(RUNNING_FLAG).trim().parse::<i32>().ok() == Some(mine) {
        let _ = DirAccess::remove_absolute(&godot::classes::ProjectSettings::singleton().globalize_path(RUNNING_FLAG));
    }
}

/// Keep only the file name of a path (debug images in crash reports).
fn file_name_only(path: &str) -> String { path.rsplit(['/', '\\']).next().unwrap_or(path).to_string() }

fn strip_event(mut event: sentry::protocol::Event<'static>) -> Option<sentry::protocol::Event<'static>> {
    use sentry::protocol::DebugImage;
    event.server_name = None;
    event.user = None;
    let meta = event.debug_meta.to_mut();
    for image in &mut meta.images {
        match image {
            DebugImage::Apple(a) => a.name = file_name_only(&a.name),
            DebugImage::Symbolic(s) => { s.name = file_name_only(&s.name); s.debug_file = s.debug_file.as_deref().map(file_name_only); }
            DebugImage::Wasm(w) => { w.name = file_name_only(&w.name); w.code_file = file_name_only(&w.code_file); w.debug_file = w.debug_file.as_deref().map(file_name_only); }
            _ => {}
        }
    }
    Some(event)
}

#[godot_api]
impl BpApp {
    /// Crate version, for example `"0.1.0"`.
    #[func]
    fn version(&self) -> GString { GString::from(env!("CARGO_PKG_VERSION")) }
    /// Build facts: `{ "version": String, "build_id": String, "protocol": int, "blueprint_format": int,
    /// "save_format": int, "replay_format": int, "debug": bool, "steam": bool, "luau": bool }`.
    /// `build_id` is the `BP_BUILD_ID` environment variable at compile time (CI sets it to the tag and
    /// commit), `"local"` otherwise.
    #[func]
    fn build_info(&self) -> VarDictionary {
        crate::convert::d! {
            "version" => GString::from(env!("CARGO_PKG_VERSION")),
            "build_id" => GString::from(option_env!("BP_BUILD_ID").unwrap_or("local")),
            "protocol" => sim_systems::protocol::PROTOCOL_VERSION as i64,
            "blueprint_format" => content::blueprint::BLUEPRINT_FORMAT as i64,
            "save_format" => content::save::SAVE_FORMAT as i64,
            "replay_format" => content::replay::REPLAY_FORMAT as i64,
            "debug" => cfg!(debug_assertions) || Os::singleton().is_debug_build(),
            "steam" => cfg!(feature = "steam"),
            // The Luau host is compiled in only with the `luau` feature (L4, later phase).
            "luau" => cfg!(feature = "luau"),
        }
    }
    /// Parsed command-line user arguments: `{ "smoke": bool, "smoke_sortie": bool, "benchmark": bool, "replay": String,
    /// "join": String (join code), "scripted": String, "capture": String, "safe_mode": bool }`.
    #[func]
    fn command_line(&self) -> VarDictionary {
        let c = user_command_line();
        crate::convert::d! {
            "smoke" => c.smoke, "smoke_sortie" => c.smoke_sortie, "benchmark" => c.benchmark, "replay" => GString::from(c.replay.as_str()), "join" => GString::from(c.join.as_str()),
            "scripted" => GString::from(c.scripted.as_str()), "capture" => GString::from(c.capture.as_str()), "safe_mode" => c.safe_mode,
        }
    }
    /// Record what the renderer started as (L1270). `report` keys, read by `boot.gd` from `RenderingServer`:
    /// `rendering_method: String`, `driver: String`, `adapter: String`, `api_version: String`,
    /// `fallbacks: PackedStringArray`. Writes the startup log line and keeps the values for crash and
    /// benchmark reports. Returns true when the game is in safe mode (Compatibility renderer), in which case
    /// the UI shows the persistent update-your-driver notice.
    #[func]
    fn log_startup(&mut self, report: VarDictionary) -> bool {
        let text = |k: &str| crate::convert::get_text(&report, k).unwrap_or_default().chars().take(200).collect::<String>();
        let fallbacks: Vec<String> = report.get("fallbacks").and_then(|v| v.try_to::<PackedStringArray>().ok()).map(|a| a.as_slice().iter().take(16).map(|s| s.to_string()).collect()).unwrap_or_default();
        let r = StartupReport { rendering_method: text("rendering_method"), driver: text("driver"), adapter: text("adapter"), api_version: text("api_version"), fallbacks };
        godot_print!("{}", build_line());
        godot_print!("{}", r.log_line());
        if r.safe_mode() { godot_warn!("renderer: Compatibility safe mode; the GPU driver lacks Direct3D 12 and Vulkan support"); }
        let safe = r.safe_mode();
        self.set_tag_rust("renderer", &r.rendering_method);
        self.set_tag_rust("driver", &r.driver);
        self.startup = Some(r);
        safe
    }
    /// The stored startup report with `safe_mode: bool` added.
    #[func]
    fn startup_report(&self) -> VarDictionary {
        let r = self.startup.clone().unwrap_or_default();
        let fallbacks: PackedStringArray = r.fallbacks.iter().map(|s| GString::from(s.as_str())).collect();
        crate::convert::d! {
            "rendering_method" => GString::from(r.rendering_method.as_str()), "driver" => GString::from(r.driver.as_str()),
            "adapter" => GString::from(r.adapter.as_str()), "api_version" => GString::from(r.api_version.as_str()),
            "fallbacks" => fallbacks, "safe_mode" => r.safe_mode(),
        }
    }
    /// The install's random client id as 16 hex digits; created on first call with
    /// `net::join::ClientIdentity::generate` (never 0 or `u64::MAX`) and kept in `user://identity.ron`
    /// (L1003). It is the `client_id` of `link::net::LinkOptions::identity` and of every hello; the display
    /// name beside it comes from the settings (`Lobby.set_display_name`).
    #[func]
    fn client_id(&self) -> GString { GString::from(format!("{:016x}", install_client_id()).as_str()) }
    /// User data folders, created if missing: `{ "blueprints": String, "kits": String, "saves": String,
    /// "replays": String, "logs": String, "bench": String, "captures": String }` (`user://` paths).
    #[func]
    fn user_dirs(&self) -> VarDictionary {
        let mut d = VarDictionary::new();
        for (key, path) in USER_DIRS {
            if !ensure_dir(path) { godot_warn!("app: could not create {path}"); }
            d.set(key, &GString::from(path).to_variant());
        }
        d
    }
    /// Third-party licence texts for the credits screen: `Array[Dictionary]` with `name: String`,
    /// `licence: String`, `text: String` (fonts under OFL, Rust crates, Godot, chart colours credit).
    #[func]
    fn licences(&self) -> Array<VarDictionary> {
        let mut out: Array<VarDictionary> = Array::new();
        let mut push = |name: &str, licence: &str, text: String| out.push(&crate::convert::d! { "name" => GString::from(name), "licence" => GString::from(licence), "text" => GString::from(text.as_str()) });
        push("Godot Engine", "MIT", Engine::singleton().get_license_text().to_string());
        let ofl = "res://ui/fonts/licenses/OFL.txt";
        let ofl_text = if FileAccess::file_exists(ofl) { FileAccess::get_file_as_string(ofl).to_string() } else { "SIL Open Font License 1.1: https://openfontlicense.org".to_string() };
        push("Atkinson Hyperlegible Next and Atkinson Hyperlegible Mono (Braille Institute of America)", "OFL-1.1", ofl_text);
        for (name, licence) in RUST_CRATES {
            push(name, licence, format!("{name} is used under the {licence} licence. The full licence texts are in the LICENSES folder that ships with the game."));
        }
        push("Chart colours", "Public domain", "Chart colours follow NOAA Chart 18649, Entrance to San Francisco Bay, a public-domain work of the NOAA Office of Coast Survey.".to_string());
        out
    }

    // ---- Crash reports (L1330-1347) ----

    /// Start Rust panic reporting after the player consented and GDScript initialised the Godot SDK with the
    /// same values. Nothing is written or sent before this call.
    #[func]
    fn crash_reports_init(&mut self, dsn: GString, release: GString, dist: GString, environment: GString) {
        if self.sentry_guard.is_some() { return; }
        // No DSN configured (every build until the owner supplies one): nothing to start, nothing sent.
        if dsn.to_string().trim().is_empty() { godot_print!("crash reports: no DSN in this build; nothing is sent"); return; }
        let dsn: Option<sentry::types::Dsn> = match dsn.to_string().trim().parse() { Ok(d) => Some(d), Err(_) => { godot_warn!("crash reports: the DSN does not parse; Rust panics are not reported"); return; } };
        let text = |g: &GString| { let s = g.to_string(); (!s.trim().is_empty()).then(|| std::borrow::Cow::Owned(s)) };
        let dist = text(&dist);
        let mut options = sentry::ClientOptions::default();
        options.dsn = dsn;
        options.release = text(&release);
        options.environment = text(&environment);
        options.send_default_pii = false;
        // An empty name stops the contexts integration from filling in the host name.
        options.server_name = Some(std::borrow::Cow::Borrowed(""));
        options.attach_stacktrace = true;
        options.auto_session_tracking = false;
        // The crate has no `dist` option: it goes on each event, beside the stripping.
        options.before_send = Some(std::sync::Arc::new(move |mut event: sentry::protocol::Event<'static>| {
            if event.dist.is_none() { event.dist = dist.clone(); }
            strip_event(event)
        }));
        let guard = sentry::init(options);
        if !guard.is_enabled() { godot_warn!("crash reports: the Rust client did not start"); return; }
        self.sentry_guard = Some(guard);
        self.crash_reports_on = true;
        let tags = self.tags.clone();
        sentry::configure_scope(|scope| { for (k, v) in &tags { scope.set_tag(k, v); } });
    }
    /// Withdraw consent: close the Rust client. GDScript closes the Godot SDK and deletes its files.
    #[func]
    fn crash_reports_close(&mut self) {
        if let Some(guard) = self.sentry_guard.take() {
            // Withdrawn consent: drop what is queued rather than sending it.
            if let Some(client) = sentry::Hub::current().client() { client.close(Some(std::time::Duration::from_millis(0))); }
            drop(guard);
        }
        self.crash_reports_on = false;
    }
    /// Set one tag in both SDKs (renderer, driver, tier, role, build id).
    #[func]
    fn crash_reports_set_tag(&mut self, key: GString, value: GString) {
        let (k, v) = (key.to_string(), value.to_string());
        if k.is_empty() { return; }
        self.set_tag_rust(&k, &v);
        let mut engine = Engine::singleton();
        if engine.has_singleton("SentrySDK") {
            if let Some(mut sdk) = engine.get_singleton("SentrySDK") {
                if sdk.has_method("set_tag") { sdk.call("set_tag", &[key.to_variant(), value.to_variant()]); }
            }
        }
        let _ = &mut engine;
    }
    /// Debug builds only: panic once, on the main thread or on a sim worker, to test reporting (L1343).
    #[func]
    fn debug_panic(&mut self, on_worker: bool) {
        if !(cfg!(debug_assertions) || Os::singleton().is_debug_build()) { godot_warn!("debug_panic is only available in debug builds"); return; }
        if on_worker {
            if self.panicked_on_worker { return; }
            self.panicked_on_worker = true;
            // A plain named thread: a panic on a rayon pool thread would abort the process.
            let handle = std::thread::Builder::new().name("bp-debug-worker".into()).spawn(|| panic!("debug panic on a worker thread (crash-report test)"));
            if let Ok(h) = handle { let _ = h.join(); }
        } else {
            if self.panicked_on_main { return; }
            self.panicked_on_main = true;
            panic!("debug panic on the main thread (crash-report test)");
        }
    }

    // ---- Playtest logs and diagnostics (L80, L881, L2209, L1039) ----

    /// The timing log as plain text for the clipboard (L80): a build line, one row per core loop (the loop
    /// in progress last, marked `incomplete`), then the tutorial's step log. `""` when there is nothing yet.
    /// Never sent anywhere. The rows are written by `process`, which follows the campaign phase.
    #[func]
    fn timing_log_text(&self) -> GString {
        let mut body = read_text(TIMING_LOG);
        if let Some(open) = &self.open_loop { body.push_str(&open.timing_row(false)); }
        let tutorial = read_text(TUTORIAL_LOG);
        if !tutorial.trim().is_empty() { body.push_str(&format!("tutorial log:\n{tutorial}")); }
        GString::from(copy_text(&build_line(), "timing log, seconds per step", &body).as_str())
    }
    /// The operation log as plain text for the clipboard (L881): loop times, income, spend and ghost results
    /// per loop. `""` when there is nothing yet.
    #[func]
    fn operation_log_text(&self) -> GString {
        let mut body = read_text(OPERATION_LOG);
        if let Some(open) = &self.open_loop { body.push_str(&open.operation_row(false)); }
        GString::from(copy_text(&build_line(), "operation log", &body).as_str())
    }
    /// The playtest report as one text (L2209): build, timing, operation and tutorial logs. The same text is
    /// written to `user://logs/playtest_report.txt` whenever a loop closes and when the game quits.
    #[func]
    fn playtest_report_text(&self) -> GString { GString::from(self.session_report().as_str()) }
    /// The logs folder as an absolute path, for "Open log folder" and the crash notice. Holds `godot.log`
    /// (this run), `godot<date>.log` (earlier runs, a crashed one included), the playtest logs and the
    /// connection log.
    #[func]
    fn log_folder(&self) -> GString {
        ensure_dir("user://logs");
        godot::classes::ProjectSettings::singleton().globalize_path("user://logs")
    }
    /// True when the previous run crashed or was killed: boot then tells the player where its log is.
    #[func]
    fn previous_run_crashed(&self) -> bool { self.previous_run_crashed }
    /// Diagnostics as plain text for a bug report: build, startup report, settings that differ from
    /// defaults, the connection log. No user names or absolute paths.
    #[func]
    fn diagnostics_text(&self) -> GString {
        use std::fmt::Write;
        let mut t = String::new();
        let os = Os::singleton();
        let _ = writeln!(t, "Black Pennant {} (build {}, protocol {})", env!("CARGO_PKG_VERSION"), option_env!("BP_BUILD_ID").unwrap_or("local"), sim_systems::protocol::PROTOCOL_VERSION);
        let _ = writeln!(t, "os: {} {}; processor: {}; model: {}", os.get_name(), os.get_version(), os.get_processor_name(), os.get_model_name());
        let _ = writeln!(t, "previous run closed normally: {}", if self.previous_run_crashed { "no" } else { "yes" });
        if let Some(r) = &self.startup { let _ = writeln!(t, "{}", r.log_line()); } else { let _ = writeln!(t, "renderer: not logged yet"); }
        let quality = crate::services::services().quality.borrow().clone();
        let _ = writeln!(t, "quality: tier {:?}, scale {:.2}, degrade step {}", quality.tier, quality.base_scale * quality.dynamic_scale, quality.degrade_step);
        let _ = writeln!(t, "thermal state: {}; DPI awareness: {}", crate::platform::thermal_state(), crate::platform::dpi_awareness());
        let _ = writeln!(t, "settings that differ from defaults:");
        {
            let services = crate::services::services();
            let s = services.settings.borrow();
            let mut any = false;
            for def in crate::settings::SCHEMA {
                let Some(v) = s.values.get(def.key) else { continue };
                if *v == def.default || def.key == "graphics.detected_machine" { continue; }
                any = true;
                let shown = match v { crate::settings::SettingValue::Bool(b) => b.to_string(), crate::settings::SettingValue::Int(i) => i.to_string(), crate::settings::SettingValue::Float(f) => format!("{f:.2}"), crate::settings::SettingValue::Text(_) => "(text)".into() };
                let _ = writeln!(t, "  {} = {}", def.key, shown);
            }
            if !s.bindings.is_empty() { any = true; let _ = writeln!(t, "  {} rebound actions", s.bindings.len()); }
            if !any { let _ = writeln!(t, "  none"); }
        }
        let _ = writeln!(t, "connection log:");
        if FileAccess::file_exists(CONNECTION_LOG) {
            let log = FileAccess::get_file_as_string(CONNECTION_LOG).to_string();
            // The last 400 lines are plenty for one session and keep the report pasteable.
            let lines: Vec<&str> = log.lines().collect();
            for line in &lines[lines.len().saturating_sub(400)..] { let _ = writeln!(t, "  {line}"); }
        } else {
            let _ = writeln!(t, "  empty");
        }
        let home = if cfg!(windows) { os.get_environment("USERPROFILE") } else { os.get_environment("HOME") }.to_string();
        let user_dir = os.get_user_data_dir().to_string();
        GString::from(scrub(&t, &home, &user_dir).as_str())
    }
}

/// Rust crates in the shipped library and their licences, for the credits screen.
const RUST_CRATES: &[(&str, &str)] = &[
    ("godot-rust (gdext)", "MPL-2.0"), ("rapier3d-f64", "Apache-2.0"), ("nalgebra", "Apache-2.0"), ("glam", "MIT OR Apache-2.0"),
    ("renet2 and renet2_netcode", "MIT OR Apache-2.0"), ("igd-next", "MIT"), ("crab_nat", "MIT OR Apache-2.0"), ("netdev", "MIT"),
    ("tokio", "MIT"), ("binary-greedy-meshing", "MIT OR Apache-2.0"), ("fundsp", "MIT OR Apache-2.0"), ("libm", "MIT OR Apache-2.0"),
    ("rayon", "MIT OR Apache-2.0"), ("serde", "MIT OR Apache-2.0"), ("ron", "MIT OR Apache-2.0"), ("bincode", "MIT"), ("base64", "MIT OR Apache-2.0"),
    ("miniz_oxide", "MIT OR Zlib OR Apache-2.0"), ("zstd", "MIT"), ("indexmap", "MIT OR Apache-2.0"), ("bitflags", "MIT OR Apache-2.0"),
    ("thiserror", "MIT OR Apache-2.0"), ("hound", "Apache-2.0"), ("blake3", "CC0-1.0 OR Apache-2.0"), ("pathfinding", "MIT OR Apache-2.0"),
    ("dubins_paths", "MIT"), ("sentry", "MIT"), ("rustls", "MIT OR Apache-2.0 OR ISC"), ("ring", "Apache-2.0 AND ISC"), ("ureq", "MIT OR Apache-2.0"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_flags_and_values() {
        let a = |v: &[&str]| parse_command_line(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["--smoke"]), CommandLine { smoke: true, ..Default::default() });
        // --smoke-sortie is its own flag, not --smoke.
        assert_eq!(a(&["--smoke-sortie"]), CommandLine { smoke_sortie: true, ..Default::default() });
        let c = a(&["--replay=user://replays/a.bpr", "--join", "ABCD-1234", "--benchmark", "--safe-mode", "--capture", "--scripted=coop"]);
        assert_eq!((c.replay.as_str(), c.join.as_str(), c.benchmark, c.safe_mode, c.capture.as_str(), c.scripted.as_str()), ("user://replays/a.bpr", "ABCD-1234", true, true, "", "coop"));
        assert_eq!(a(&["--nonsense", "x"]), CommandLine::default());
    }

    #[test]
    fn identity_round_trips() {
        assert_eq!(parse_identity(&identity_ron(0x1234_5678_9abc_def0)), Some(0x1234_5678_9abc_def0));
        assert_eq!(parse_identity("(client_id: 0)"), None);
        assert_eq!(parse_identity(&format!("(client_id: {})", u64::MAX)), None);
        assert_eq!(parse_identity("garbage"), None);
        assert_eq!(parse_identity("(client_id: 99999999999999999999999)"), None);
    }

    #[test]
    fn active_clock_drops_long_pauses_and_suspends() {
        let mut c = ActiveClock::default();
        for _ in 0..60 { c.advance(1.0, false); }
        // A 30 s pause counts.
        for _ in 0..30 { c.advance(1.0, true); }
        c.advance(1.0, false);
        assert!((c.now() - 91.0).abs() < 1e-9);
        // A 10 minute pause does not.
        for _ in 0..600 { c.advance(1.0, true); }
        c.advance(1.0, false);
        assert!((c.now() - 92.0).abs() < 1e-9);
        // Nor does a 30 s frame gap (the app was suspended).
        c.advance(30.0, false);
        assert!((c.now() - 92.0).abs() < 1e-9);
    }

    fn sample_loop() -> OpenLoop {
        let mut l = OpenLoop { started: "2026-11-14T10:00:00".into(), build: "0.1.0 (build local)".into(), campaign: "2a".into(), operation: "op_7".into(), template: "convoy_raid".into(), difficulty: "normal".into(), salvage: Some(100), ..Default::default() };
        // Walk the phases as the campaign reports them, one second per frame.
        use crate::enums::CampaignPhase as P;
        let frames: &[(P, bool, u32)] = &[(P::Briefing, false, 60), (P::DryDock, false, 240), (P::DryDock, true, 90), (P::Sortie, false, 1200), (P::AfterAction, false, 150), (P::Chart, false, 100)];
        for (phase, splash, secs) in frames {
            for _ in 0..*secs { l.add_time(loop_step(*phase, *splash).unwrap(), 1.0); }
        }
        l.salvage_now(40);   // refit
        l.salvage_now(400);  // payout
        l.sortied = true;
        l.first_attempt = Some(true);
        l.outcome = Some("victory".into());
        (l.ghosts_met, l.ghosts_total) = (1, 3);
        l
    }

    #[test]
    fn loop_rows_carry_every_field_the_spec_lists() {
        let l = sample_loop();
        assert_eq!(l.timing_row(true), "2026-11-14T10:00:00\tbuild=0.1.0 (build local)\tcampaign=2a\toperation=op_7\ttemplate=convoy_raid\tdifficulty=normal\tbriefing=60\trefit=240\tsplash=90\tsortie=1200\trecover=150\tchart=100\tloop=1840\tfirst_attempt=yes\toutcome=victory\n");
        assert_eq!(l.operation_row(false), "2026-11-14T10:00:00\tbuild=0.1.0 (build local)\tcampaign=2a\toperation=op_7\tloop=1840\tsortie=1200\tincome=360\tspend=60\tghosts_met=1/3\toutcome=victory\tincomplete\n");
    }

    #[test]
    fn loops_open_on_a_new_briefing_and_steps_map_from_phases() {
        use crate::enums::CampaignPhase as P;
        assert!(starts_new_loop(None, "a"));
        let mut l = OpenLoop { operation: "a".into(), ..Default::default() };
        assert!(!starts_new_loop(Some(&l), "a"), "back to the same briefing from the dry dock");
        assert!(starts_new_loop(Some(&l), "b"));
        l.sortied = true;
        assert!(starts_new_loop(Some(&l), "a"), "a retry after a sortie is a new loop");
        assert_eq!(loop_step(P::Advancing, false), Some(LoopStep::Chart));
        assert_eq!(loop_step(P::DryDock, true), Some(LoopStep::Splash));
        assert_eq!(loop_step(P::None, false), None);
        // A pause the clock takes back comes off the step and never below zero.
        l.add_time(LoopStep::Sortie, 50.0);
        l.add_time(LoopStep::Sortie, -120.0);
        assert_eq!(l.step_s[3], 0.0);
    }

    #[test]
    fn first_attempts_read_back_from_the_operation_log() {
        let l = sample_loop();
        let mut never = l.clone();
        (never.operation, never.outcome) = ("op_8".into(), None);
        let log = format!("{}{}", l.operation_row(true), never.operation_row(false));
        let keys = attempted_keys(&log);
        assert!(keys.contains("2a/op_7") && !keys.contains("2a/op_8"), "{keys:?}");
        assert_eq!(template_name(&content::campaign::operation::OperationKind::Template(content::campaign::operation::MissionType::HunterGroupSurvival)), "hunter_group_survival");
        assert_eq!(copy_text("b", "t", " \n"), "");
        assert!(copy_text("b", "t", "row").starts_with("b\nt\nrow"));
    }

    #[test]
    fn diagnostics_scrub_paths_and_names() {
        let s = scrub("log at /Users/tester/Library/x and C:\\Users\\tester\\y\n(slot: 1, name: \"Tester\", ping: 40)\n(name: \"a\")", "/Users/tester", "/Users/tester/Library/x");
        assert!(!s.contains("tester") && !s.contains("Tester"), "{s}");
        assert!(s.contains("<user>") && s.contains("name: \"-\""));
    }

    #[test]
    fn crash_events_lose_host_user_and_folders() {
        use sentry::protocol::{DebugImage, Event, SymbolicDebugImage, User};
        let mut e = Event::default();
        e.server_name = Some("players-laptop".into());
        e.user = Some(User { username: Some("someone".into()), ..Default::default() });
        e.debug_meta.to_mut().images.push(DebugImage::Symbolic(SymbolicDebugImage { name: "/Users/someone/game/libgdclient.dylib".into(), arch: None, image_addr: 0.into(), image_size: 0, image_vmaddr: 0.into(), id: Default::default(), code_id: None, debug_file: Some("/Users/someone/game/libgdclient.dylib.dSYM".into()) }));
        let e = strip_event(e).unwrap();
        assert!(e.server_name.is_none() && e.user.is_none());
        match &e.debug_meta.images[0] { DebugImage::Symbolic(s) => { assert_eq!(s.name, "libgdclient.dylib"); assert_eq!(s.debug_file.as_deref(), Some("libgdclient.dylib.dSYM")); } _ => unreachable!() }
    }
}
