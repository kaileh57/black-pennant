//! Unit `gd-app`. `BpApp` (autoload `App`): process-level services.
//! DESIGN.md L1270 (startup log of rendering method, driver, adapter, API version and every fallback; values
//! go into crash and benchmark reports; safe mode notice), L1330-1347 (crash reports: Sentry, opt-in, Rust
//! panics through the `sentry` crate after consent, one function sets each tag in both SDKs; a debug command
//! panics once on the main thread and once on a worker), L1003 (random 64-bit client id per install),
//! L80 (local playtest timing log: never sent; Settings > Playtest > Copy timing log), L1039 (diagnostics
//! bundle with the connection log), L1359-1360 (user data paths).
//! The Sentry Godot SDK is driven from GDScript (`res://boot/consent.gd`); this class owns the Rust side,
//! the `sentry` crate line of L1338: panics only, no minidumps, no release health. Events carry no host
//! name, user, or folder of the player: `before_send` strips `server_name`, `user` and the folders of
//! debug-image paths (symbolication uses the debug id, not the path).
//!
//! Files: `user://identity.ron` (client id), `user://logs/timing_log.txt` (playtest timing rows, appended
//! when a loop closes), [`CONNECTION_LOG`] (written by the lobby, read here for diagnostics).
use godot::classes::file_access::ModeFlags;
use godot::classes::{DirAccess, Engine, FileAccess, Os, Time};
use godot::obj::Singleton;
use godot::prelude::*;
use std::collections::BTreeMap;

/// The local connection log the lobby appends to (L1039).
pub const CONNECTION_LOG: &str = "user://logs/connection.log";
pub const TIMING_LOG: &str = "user://logs/timing_log.txt";
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
/// The steps of one core loop, in order (`timing_mark`).
pub const TIMING_STEPS: [&str; 8] = ["briefing_open", "refit_done", "splash_start", "splash_end", "sortie_start", "sortie_end", "recover_done", "chart_done"];
/// Exit budget (doc of `exit_tree`).
const EXIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);
const CRASH_FLUSH_MAX: std::time::Duration = std::time::Duration::from_secs(2);

// ---------------------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------------------

/// Parsed user arguments (after `--` on the command line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandLine { pub smoke: bool, pub benchmark: bool, pub replay: String, pub join: String, pub scripted: String, pub capture: String, pub safe_mode: bool }

/// `--smoke`, `--benchmark`, `--safe-mode`, and `--replay`, `--join`, `--scripted`, `--capture` with a value
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

/// One loop of the timing log as a text row: loop start (local date and time) and the seconds each step
/// took, named by the mark that ended it. `marks` are (step, active seconds) in arrival order.
pub fn timing_row(started: &str, marks: &[(String, f64)], complete: bool) -> String {
    let mut row = format!("{started}");
    for pair in marks.windows(2) {
        let secs = (pair[1].1 - pair[0].1).max(0.0);
        row.push_str(&format!("\t{}={:.0}", pair[1].0, secs));
    }
    if let (Some(start), Some(sortie_end)) = (marks.iter().find(|m| m.0 == "sortie_start"), marks.iter().rev().find(|m| m.0 == "sortie_end")) {
        row.push_str(&format!("\tsortie_length={:.0}", (sortie_end.1 - start.1).max(0.0)));
    }
    if let (Some(first), Some(last)) = (marks.first(), marks.last()) { row.push_str(&format!("\tloop={:.0}", (last.1 - first.1).max(0.0))); }
    if !complete { row.push_str("\tincomplete"); }
    row.push('\n');
    row
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
    pub(crate) timing_marks: Vec<(String, f64)>,
    pub(crate) startup: Option<StartupReport>,
    pub(crate) sentry_guard: Option<sentry::ClientInitGuard>,
    pub(crate) tags: BTreeMap<String, String>,
    pub(crate) clock: ActiveClock,
    pub(crate) loop_started: String,
    pub(crate) panicked_on_main: bool,
    pub(crate) panicked_on_worker: bool,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpApp {
    fn init(base: Base<Node>) -> Self {
        Self { crash_reports_on: false, timing_marks: Vec::new(), startup: None, sentry_guard: None, tags: BTreeMap::new(), clock: ActiveClock::default(), loop_started: String::new(), panicked_on_main: false, panicked_on_worker: false, base }
    }
    /// Advance the timing clock; the sortie pause flag comes from this frame's snapshot.
    fn process(&mut self, delta: f64) {
        let paused = crate::services::services().feed.borrow().snapshot.as_ref().is_some_and(|s| s.pacing.paused);
        self.clock.advance(delta, paused);
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
        if self.timing_marks.len() > 1 { self.write_timing_row(false); }
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
    fn write_timing_row(&mut self, complete: bool) {
        let marks = std::mem::take(&mut self.timing_marks);
        if marks.len() < 2 { return; }
        let row = timing_row(&self.loop_started, &marks, complete);
        ensure_dir("user://logs");
        let file = if FileAccess::file_exists(TIMING_LOG) { FileAccess::open(TIMING_LOG, ModeFlags::READ_WRITE) } else { FileAccess::open(TIMING_LOG, ModeFlags::WRITE) };
        match file {
            Some(mut f) => { f.seek_end(); f.store_string(&row); f.close(); }
            None => { godot_warn!("app: could not write {TIMING_LOG}"); }
        }
    }
    fn set_tag_rust(&mut self, key: &str, value: &str) {
        self.tags.insert(key.to_string(), value.to_string());
        if self.sentry_guard.is_some() { sentry::configure_scope(|scope| scope.set_tag(key, value)); }
    }
}

fn ensure_dir(path: &str) -> bool { DirAccess::dir_exists_absolute(path) || DirAccess::make_dir_recursive_absolute(path) == godot::global::Error::OK }

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
    /// Parsed command-line user arguments: `{ "smoke": bool, "benchmark": bool, "replay": String,
    /// "join": String (join code), "scripted": String, "capture": String, "safe_mode": bool }`.
    #[func]
    fn command_line(&self) -> VarDictionary {
        let args: Vec<String> = Os::singleton().get_cmdline_user_args().as_slice().iter().map(|s| s.to_string()).collect();
        let c = parse_command_line(&args);
        crate::convert::d! {
            "smoke" => c.smoke, "benchmark" => c.benchmark, "replay" => GString::from(c.replay.as_str()), "join" => GString::from(c.join.as_str()),
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

    // ---- Playtest timing log and diagnostics (L80, L1039) ----

    /// Mark a step of the core loop for the local timing log: `&"briefing_open"`, `&"refit_done"`,
    /// `&"splash_start"`, `&"splash_end"`, `&"sortie_start"`, `&"sortie_end"`, `&"recover_done"`,
    /// `&"chart_done"`. Pauses over 2 minutes and suspended time are excluded when rows are written.
    /// `briefing_open` starts a loop and `chart_done` closes it and appends its row to the log file.
    #[func]
    fn timing_mark(&mut self, step: StringName) {
        if !crate::services::services().settings.borrow().bool("playtest.timing_log") { return; }
        let step = step.to_string();
        if !TIMING_STEPS.contains(&step.as_str()) { godot_warn!("timing_mark: unknown step {step}"); return; }
        if step == "briefing_open" {
            if self.timing_marks.len() > 1 { self.write_timing_row(false); }
            self.timing_marks.clear();
            self.loop_started = Time::singleton().get_datetime_string_from_system().to_string();
        }
        if self.timing_marks.is_empty() && self.loop_started.is_empty() { self.loop_started = Time::singleton().get_datetime_string_from_system().to_string(); }
        self.timing_marks.push((step.clone(), self.clock.now()));
        if step == "chart_done" { self.write_timing_row(true); self.loop_started.clear(); }
    }
    /// The timing log as plain text for the clipboard. Never sent anywhere.
    #[func]
    fn timing_log_text(&self) -> GString {
        let mut text = String::from("loop started\tstep=seconds...\n");
        if FileAccess::file_exists(TIMING_LOG) { text.push_str(&FileAccess::get_file_as_string(TIMING_LOG).to_string()); }
        if self.timing_marks.len() > 1 { text.push_str(&timing_row(&self.loop_started, &self.timing_marks, false)); }
        GString::from(text.as_str())
    }
    /// Diagnostics as plain text for a bug report: build, startup report, settings that differ from
    /// defaults, the connection log. No user names or absolute paths.
    #[func]
    fn diagnostics_text(&self) -> GString {
        use std::fmt::Write;
        let mut t = String::new();
        let os = Os::singleton();
        let _ = writeln!(t, "Black Pennant {} (build {}, protocol {})", env!("CARGO_PKG_VERSION"), option_env!("BP_BUILD_ID").unwrap_or("local"), sim_systems::protocol::PROTOCOL_VERSION);
        let _ = writeln!(t, "os: {} {}; processor: {}; model: {}", os.get_name(), os.get_version(), os.get_processor_name(), os.get_model_name());
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

    #[test]
    fn timing_rows_name_each_step() {
        let marks: Vec<(String, f64)> = [("briefing_open", 0.0), ("refit_done", 95.0), ("sortie_start", 120.0), ("sortie_end", 720.0), ("chart_done", 800.0)].iter().map(|(s, t)| (s.to_string(), *t)).collect();
        let row = timing_row("2026-11-14T10:00:00", &marks, true);
        assert_eq!(row, "2026-11-14T10:00:00\trefit_done=95\tsortie_start=25\tsortie_end=600\tchart_done=80\tsortie_length=600\tloop=800\n");
        assert!(timing_row("x", &marks[..2], false).ends_with("\tincomplete\n"));
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
