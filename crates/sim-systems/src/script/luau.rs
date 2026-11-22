//! Unit `sys-script-luau` (Phase::L4, behind the `luau` cargo feature): the mlua-backed [`ScriptHost`].
//! DESIGN.md L355-390 (L4 scripting: VM setup order, limits, fault handling), L2115-2148 (script safety,
//! fuzz targets and oracles). mlua is pinned `=0.12.2` with `luau` only (never `luau-jit`), plus `send`
//! because the session's host moves to the sim thread and runs on the sim pool (L357).
//!
//! # The script contract (what a player writes)
//!
//! A script node's source is one Luau chunk. The chunk runs once when the design loads (dry dock, sortie
//! start, splash re-arm) and returns the node's tick function; a chunk that returns nothing may instead
//! define a global function named `tick`. The tick function runs at the 30 Hz graph rate with the node's
//! inputs as positional arguments, in declared port order, and returns its outputs the same way:
//!
//! ```lua
//! local last = 0
//! return function(target_pos, speed_kn)        -- inputs: Vec3, Scalar
//!     local b = bearing_to(target_pos)          -- host API, see `script::host_api`
//!     if b == nil then return nil, speed_kn end -- nil: "no value", the officer stage's own output stands
//!     last = b
//!     return b, speed_kn                        -- outputs: Angle, Scalar
//! end
//! ```
//!
//! - Locals and upvalues of the chunk persist across ticks for the whole sortie: filters, latches and state
//!   machines live there. Each script has its own global environment; scripts of the same vessel share one
//!   VM (one memory limit, one interrupt budget) but never see each other's globals.
//! - Values: `nil` is "no value" both ways. Bool is a boolean; Scalar and Angle are numbers (angles in
//!   radians, compass headings 0 = north, clockwise); Vec3 is a native Luau `vector` (f32 components, world
//!   metres; outputs also accept a table `{x=, y=, z=}` or `{x, y, z}`); Text is a string (outputs keep the
//!   first 1 KiB); Stance is `"friend"`, `"neutral"`, `"suspect"` or `"hostile"` and Emcon `"silent"`,
//!   `"burst"` or `"active"` (outputs also accept the 0-based index); Track is the track number; TrackList is
//!   an array of track numbers; PartState is the part's blueprint slot number; Orders is `true`.
//!   DetectionList is an array of detection tables (`sensor`, `kind`, `bearing`, `range_m`, `height_m`,
//!   `sigma_range_m`, `sigma_bearing`, `domain`); a DetectionList output passes through, filtered and
//!   reordered, the detection tables the script received this tick (other entries are dropped). Lists keep
//!   their first 1,024 entries. A returned value that does not fit its port's type is "no value".
//! - Host API functions (`own_position()`, `track_count()` ...) are globals, callable only inside the tick
//!   function. Each call charges its declared work units to the interrupt budget. A call with the wrong
//!   number or kind of arguments is a script bug and greys the script out, even inside `pcall`.
//! - The standard library is Luau's safe set without `os`, `debug`, `require`, `loadstring`, `getfenv`,
//!   `setfenv`, `gcinfo`, `collectgarbage` and `newproxy`. `print` does nothing. `math.random` is seeded
//!   from the vessel, so a run repeats. `tostring` and `string.format("%s")` name reference values by type
//!   (`"table"`) instead of printing a heap address. Builtins whose work grows with an argument
//!   (`string.rep`, `string.sub`, `string.find`, `table.create`, `table.concat`, `buffer.create` and the
//!   rest of `CHARGED_BUILTINS`) charge one work unit per 128 bytes or 8 elements before they run, so
//!   about 640 KB of builtin output fits in one tick's budget. Each interrupt also charges one unit per
//!   128 bytes the VM's memory grew since the last one, and each error a `pcall`, `xpcall` or
//!   `coroutine.resume` catches costs [`CAUGHT_ERROR_UNITS`].
//!
//! # Faults
//!
//! A throw, the memory limit, the interrupt budget and the wall-clock backstop end the run with
//! [`ScriptFault::Error`], [`ScriptFault::Memory`], [`ScriptFault::Budget`] or [`ScriptFault::Backstop`]; the
//! graph VM then discards the outputs, greys the node and lets the replaced officer stage drive its sinks
//! from the same tick (`graph::vm`). Budget, backstop and host-call faults are sticky for the run: a
//! script that catches the error with `pcall` and returns normally still faults. The interrupt callback
//! keeps failing once any of the three has tripped, so no loop survives it.
//!
//! # Setup order of every VM (L359), exactly as the fuzz targets reproduce it
//!
//! 1. `Lua::new_with(StdLib::ALL_SAFE ^ StdLib::OS, LuaOptions::default())`;
//! 2. the host removes and replaces globals (list above), installs the host API and seeds `math.random`;
//! 3. `set_memory_limit(limits.memory_bytes)`;
//! 4. `set_interrupt` with the shared budget counter;
//! 5. `sandbox(true)`, last: libraries, builtin metatables and the globals become read-only.
//!
//! Sources always load in text mode, so precompiled bytecode (unverified by Luau) is refused. A source is
//! limited to 256 KiB. The chunk's top level runs under its own fresh budget and the backstop, so an endless
//! loop there fails the load instead of hanging the sortie start.
//!
//! The 2 ms backstop is the only clock read in sim-systems (`Instant`, at an interrupt about every 16
//! work units, and at every host call). It can only stop a script at an interrupt or a host call. The charged builtins keep sized work
//! inside the budget, the memory-growth charge covers allocation between interrupts (table constructors,
//! the `..` operator), and the catch guards cover raised errors. What is left uncharged (`table.sort`
//! without a comparator, garbage a sweep frees inside one interval, a garbage-collection step, and the
//! unwinding through nested `pcall`s after the run has been stopped, at most Luau's C-call depth) is
//! bounded by the 8 MB memory limit and that depth. `cost_sweep` measures these paths. The Luau `LuauTableMoveTimeoutFix`,
//! `LuauTableRobustOom`, `LuauTableArrayAdjustCheck` and `LuauSandboxFreezesVectorMetatable` fixes are
//! switched on process-wide before the first VM is made.
//!
//! Known limits, for the fuzz gate to judge: a memory error caught by the script's own `pcall` is handled
//! by the script and is not a fault; `pairs` over a table keyed by tables or functions visits keys in heap
//! address order, which can differ between two runs of the same input.
use super::{host_api, HostApi, ScriptHost, ScriptLimits, ScriptRun};
use crate::graph::PortValue;
use crate::prelude::*;
use crate::protocol::event::ScriptFault;
use crate::sensors::{Detection, Measurement};
use content::graph::{PortType, ScriptSource};
use mlua::chunk::{ChunkMode, Compiler};
use mlua::{Function, Lua, LuaOptions, MultiValue, StdLib, Table, Value, VmState};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, Once, OnceLock};

/// Longest script source accepted, in bytes.
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
/// Most entries read from a returned list (TrackList, DetectionList).
pub const MAX_LIST: usize = 1_024;
/// Longest Text output kept, in bytes (cut at a character boundary).
pub const MAX_TEXT_BYTES: usize = 1_024;
/// The backstop clock is read on every interrupt that takes the count past a multiple of this.
const BACKSTOP_EVERY: u32 = 16;

const F_BUDGET: u8 = 1;
const F_BACKSTOP: u8 = 2;
const F_HOST: u8 = 4;
/// Every sticky flag stops the run at the next interrupt or charge. A host-call fault stops it too: the
/// script is already greyed out for the tick, and an error raised by a Rust callback is far more costly
/// than one interrupt, so a `pcall` loop must not keep paying it (fuzz crash e7e6a320).
const F_STOP: u8 = F_BUDGET | F_BACKSTOP | F_HOST;

/// Globals the host takes away before the sandbox closes (L359, L2132): module loading, code loading from
/// strings, environment access, GC control and introspection, and every timing or debug surface.
const REMOVED_GLOBALS: [&str; 9] = ["os", "debug", "require", "loadstring", "getfenv", "setfenv", "gcinfo", "collectgarbage", "newproxy"];

/// Luau fixes that are still behind (default-off) flags in the pinned Luau and close known timeout and
/// out-of-memory paths in the standard library. Process-global; set once before any VM is made.
const HARDENING_FLAGS: [&str; 4] = ["LuauTableMoveTimeoutFix", "LuauTableRobustOom", "LuauTableArrayAdjustCheck", "LuauSandboxFreezesVectorMetatable"];

/// Host prelude, run before the sandbox: `tostring` and `string.format` name reference values by type
/// instead of by heap address (an address leaks layout and differs run to run), and `print` does nothing.
const PRELUDE: &str = r##"
local rawtostring, rawformat, rawtype, rawgetmt, rawselect, rawget, rawfind = tostring, string.format, type, getmetatable, select, rawget, string.find
local refs = { table = true, ["function"] = true, userdata = true, thread = true, buffer = true }
local function safe(v)
    local t = rawtype(v)
    if refs[t] then
        -- `getmetatable` answers a `__metatable` field, so a table may claim a `__tostring` it lacks:
        -- whatever the default form would print, only the type name leaves.
        local mt = rawgetmt(v)
        if rawtype(mt) == "table" and rawget(mt, "__tostring") ~= nil then
            local s = rawtostring(v)
            if rawtype(s) == "string" and rawfind(s, "^%a+: 0x%x+$") then return t end
            return s
        end
        return t
    end
    return rawtostring(v)
end
tostring = safe
string.format = function(fmt, ...)
    local n = rawselect("#", ...)
    local plain = true
    for i = 1, n do
        if refs[rawtype((rawselect(i, ...)))] then plain = false break end
    end
    if plain then return rawformat(fmt, ...) end
    local args = table.pack(...)
    for i = 1, n do
        if refs[rawtype(args[i])] then args[i] = safe(args[i]) end
    end
    return rawformat(fmt, table.unpack(args, 1, n))
end
print = function() end
"##;

fn harden_luau() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for f in HARDENING_FLAGS { let _ = Lua::set_fflag(f, true); }
    });
}

/// Nanoseconds on a process-wide monotonic clock: the backstop's only clock (L367).
fn now_ns() -> u64 {
    static EPOCH: OnceLock<std::time::Instant> = OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// The `HostApi` of the run in progress. Set by [`LuauHost::run`] for the duration of one call into Lua
/// and cleared by [`ApiGuard`] on every exit path, so a host function called at any other time (chunk top
/// level, a coroutine resumed later) finds `None` and faults instead of touching a dangling borrow.
#[derive(Clone, Copy)]
struct ApiSlot(Option<*mut (dyn HostApi + 'static)>);
// SAFETY: the pointer is only dereferenced on the thread that set it, while `LuauHost::run` holds the
// `&mut dyn HostApi` it came from; the slot is cleared before `run` returns.
unsafe impl Send for ApiSlot {}

/// Per-VM state shared with the interrupt callback and the host functions (they must be `Send`).
struct Shared {
    /// Interrupts plus host work units charged since `begin_tick` (or since the load step began).
    used: AtomicU32,
    budget: AtomicU32,
    /// `F_*` flags of the run in progress.
    flags: AtomicU8,
    started_ns: AtomicU64,
    backstop_ns: AtomicU64,
    /// VM memory in use at the last interrupt (plus bytes a charged builtin has already paid for): the
    /// interrupt charges growth past it, see [`Vm::new`]. `usize::MAX` until the run's first interrupt, so
    /// what the host allocates to start a run or compile a chunk is never charged to the script.
    mem_mark: AtomicUsize,
    api: Mutex<ApiSlot>,
}

impl Shared {
    fn new(limits: ScriptLimits) -> Self {
        Self {
            used: AtomicU32::new(0), budget: AtomicU32::new(limits.interrupts_per_tick), flags: AtomicU8::new(0),
            started_ns: AtomicU64::new(0), backstop_ns: AtomicU64::new(limits.backstop_micros as u64 * 1_000), mem_mark: AtomicUsize::new(0), api: Mutex::new(ApiSlot(None)),
        }
    }
    fn past_backstop(&self) -> bool { now_ns().saturating_sub(self.started_ns.load(Relaxed)) > self.backstop_ns.load(Relaxed) }
    /// Charge `units` to the budget; `Err` once the budget or the backstop has tripped.
    fn charge(&self, units: u32) -> mlua::Result<()> {
        let f = self.flags.load(Relaxed);
        if f & F_STOP != 0 { return Err(stop_error(f)); }
        // One VM runs on one thread at a time, so a plain load and store is exact. A charge the budget
        // cannot pay is refused whole: the counter stops one past the budget.
        let budget = self.budget.load(Relaxed);
        let n = self.used.load(Relaxed).saturating_add(units);
        if n > budget {
            self.used.store(budget.saturating_add(1), Relaxed);
            self.flags.fetch_or(F_BUDGET, Relaxed);
            return Err(stop_error(F_BUDGET));
        }
        self.used.store(n, Relaxed);
        Ok(())
    }
    fn api(&self) -> ApiSlot { *self.api.lock().unwrap_or_else(|e| e.into_inner()) }
    fn set_api(&self, slot: ApiSlot) { *self.api.lock().unwrap_or_else(|e| e.into_inner()) = slot; }
    /// Start a run (or a load step): clear the run's flags and start the backstop clock.
    fn start(&self) { self.flags.store(0, Relaxed); self.started_ns.store(now_ns(), Relaxed); self.mem_mark.store(usize::MAX, Relaxed); }
}

fn stop_error(flags: u8) -> mlua::Error {
    if flags & F_BACKSTOP != 0 { mlua::Error::runtime("script stopped: wall-clock backstop") }
    else if flags & F_BUDGET != 0 { mlua::Error::runtime("script stopped: interrupt budget spent") }
    else { mlua::Error::runtime("script stopped: a host function was called with the wrong arguments") }
}

/// Clears the API slot when the run ends, however it ends.
struct ApiGuard<'a>(&'a Shared);
impl Drop for ApiGuard<'_> { fn drop(&mut self) { self.0.set_api(ApiSlot(None)); } }

/// One compiled script of a vehicle.
struct Loaded {
    tick: Function,
    outputs: Vec<PortType>,
}

/// One vehicle's VM.
struct Vm {
    lua: Lua,
    shared: std::sync::Arc<Shared>,
    /// One entry per source; `None` for disabled sources and sources that failed to load.
    scripts: Vec<Option<Loaded>>,
    /// The last error message per source, for the editor.
    errors: Vec<Option<String>>,
}

/// The Luau script host: one VM per vehicle, created in [`ScriptHost::load`]. Not shared across threads;
/// the Sim runs scripted ships serially (see `script` module doc).
#[derive(Default)]
pub struct LuauHost { vms: BTreeMap<EntityId, Vm> }

impl LuauHost {
    /// A host with no vehicles loaded.
    pub fn new() -> Self { harden_luau(); Self { vms: BTreeMap::new() } }
    /// The last load or run error of a vehicle's script, for the editor's script panel.
    pub fn last_error(&self, vessel: EntityId, index: u16) -> Option<&str> { self.vms.get(&vessel)?.errors.get(index as usize)?.as_deref() }
    /// Vehicles with a VM, ascending.
    pub fn vessels(&self) -> impl Iterator<Item = EntityId> + '_ { self.vms.keys().copied() }
}

/// Builtins with a Luau fast path that would bypass their charged wrapper in a `safeenv` environment.
const NO_FASTCALL: [&str; 3] = ["string.sub", "string.byte", "table.unpack"];

/// The compiler every script source goes through: Luau's defaults, with the fast paths of the charged
/// builtins switched off so a direct call (`string.sub(s, i, j)`) pays like a method call does.
fn script_compiler() -> Compiler { Compiler::new().set_disabled_builtins(NO_FASTCALL) }

/// Compile-check a script source without running it (the editor's check on every edit). `Err` carries
/// Luau's message.
pub fn check_source(source: &str) -> Result<(), String> {
    if source.len() > MAX_SOURCE_BYTES { return Err(format!("the script is longer than {} KiB", MAX_SOURCE_BYTES / 1024)); }
    harden_luau();
    script_compiler().compile(source).map(|_| ()).map_err(|e| error_text(&e))
}

fn error_text(e: &mlua::Error) -> String {
    let mut s = match e {
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::RuntimeError(m) | mlua::Error::MemoryError(m) => m.clone(),
        mlua::Error::CallbackError { cause, .. } => return error_text(cause),
        other => other.to_string(),
    };
    if s.len() > MAX_TEXT_BYTES { let cut = floor_char_boundary(&s, MAX_TEXT_BYTES); s.truncate(cut); }
    s
}

fn is_memory_error(e: &mlua::Error) -> bool {
    match e {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } => is_memory_error(cause),
        mlua::Error::WithContext { cause, .. } => is_memory_error(cause),
        _ => false,
    }
}

fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max { return s.len(); }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}

/// The fault a finished run or load step reports: the host's sticky flags first, then the error itself.
fn classify(flags: u8, result: Result<(), &mlua::Error>) -> Option<ScriptFault> {
    if flags & F_BACKSTOP != 0 { return Some(ScriptFault::Backstop); }
    if flags & F_BUDGET != 0 { return Some(ScriptFault::Budget); }
    match result {
        Err(e) if is_memory_error(e) => Some(ScriptFault::Memory),
        Err(_) => Some(ScriptFault::Error),
        Ok(()) if flags & F_HOST != 0 => Some(ScriptFault::Error),
        Ok(()) => None,
    }
}

fn vessel_seed(vessel: EntityId) -> i64 {
    // A small, stable mix of the slot and generation: two vessels never share a random stream by accident.
    let x = (vessel.index as u64) ^ ((vessel.generation as u64) << 20) ^ 0x5bd1_e995;
    ((x.wrapping_mul(0x9e37_79b9) >> 8) & 0x7fff_ffff) as i64
}

impl Vm {
    /// Build a vehicle VM in the order of L359. `Err` only when the VM itself cannot be built.
    fn new(vessel: EntityId, limits: ScriptLimits) -> mlua::Result<Self> {
        harden_luau();
        // 1. Safe libraries without os.
        let lua = Lua::new_with(StdLib::ALL_SAFE ^ StdLib::OS, LuaOptions::default())?;
        let shared = std::sync::Arc::new(Shared::new(limits));
        // 2. Remove and replace globals, install the host API, seed the generator.
        let g = lua.globals();
        lua.load(PRELUDE).set_name("=host").set_mode(ChunkMode::Text).exec()?;
        for name in REMOVED_GLOBALS { g.raw_set(name, Value::Nil)?; }
        for (i, f) in host_api().iter().enumerate() {
            g.raw_set(f.name, host_function(&lua, &shared, i as u16)?)?;
        }
        for (lib, name, cost) in CHARGED_BUILTINS {
            let Ok(t) = g.raw_get::<Table>(lib) else { continue };
            let Ok(orig) = t.raw_get::<Function>(name) else { continue };
            t.raw_set(name, charged_builtin(&lua, &shared, orig, cost)?)?;
        }
        let sh = shared.clone();
        // Once the run is stopped, an enclosing `pcall` that catches the stop error does not raise it again
        // from Rust (that path costs microseconds per catch): the next interrupt or charge ends the run.
        let caught = lua.create_function(move |_, ()| if sh.flags.load(Relaxed) & F_STOP != 0 { Ok(()) } else { sh.charge(CAUGHT_ERROR_UNITS) })?;
        lua.load(CATCH_GUARDS).set_name("=host").set_mode(ChunkMode::Text).call::<()>(caught)?;
        if let Ok(math) = g.raw_get::<Table>("math") {
            if let Ok(seed) = math.raw_get::<Function>("randomseed") { seed.call::<()>(vessel_seed(vessel))?; }
        }
        // 3. Memory limit, 4. interrupt budget, 5. sandbox last.
        lua.set_memory_limit(limits.memory_bytes.max(1))?;
        let sh = shared.clone();
        // Each interrupt pays 1 unit, plus 1 per `BYTES_PER_UNIT` the VM's memory grew since the last one.
        // Interrupts fire only at calls and loop back-edges, so a loop body that builds many tables
        // (`{{{...}}}`, one constructor per level) does all that allocating between two of them;
        // charged by its growth, it pays for it (fuzz crash e7e6a320). The count is mlua's tally of the
        // sizes Luau requests, so it repeats exactly. Luau takes small objects from its own pages, so the
        // growth shows in page-sized steps; memory the collector frees is not charged back, and garbage a
        // sweep frees inside one interval hides that much growth, so the charge is a lower bound.
        lua.set_interrupt(move |lua| {
            let (used, mark) = (lua.used_memory(), sh.mem_mark.load(Relaxed));
            // Growth under one unit carries over to the next interrupt; a drop (a sweep) moves the mark down.
            let grew = if mark == usize::MAX { 0 } else { used.saturating_sub(mark) / BYTES_PER_UNIT as usize };
            sh.mem_mark.store(if mark != usize::MAX && used > mark { mark + grew * BYTES_PER_UNIT as usize } else { used }, Relaxed);
            let before = sh.used.load(Relaxed);
            sh.charge(1u32.saturating_add(grew.min(u32::MAX as usize) as u32))?;
            // The clock is read each time the count passes a multiple of `BACKSTOP_EVERY`, however far a
            // growth charge moved it.
            if before / BACKSTOP_EVERY != sh.used.load(Relaxed) / BACKSTOP_EVERY && sh.past_backstop() {
                sh.flags.fetch_or(F_BACKSTOP, Relaxed);
                return Err(stop_error(F_BACKSTOP));
            }
            Ok(VmState::Continue)
        });
        lua.sandbox(true)?;
        Ok(Self { lua, shared, scripts: Vec::new(), errors: Vec::new() })
    }

    /// Compile one source into its own environment and run its top level under a fresh budget.
    fn load_one(&mut self, globals: &Table, src: &ScriptSource) -> Result<Loaded, (ScriptFault, String)> {
        if src.source.len() > MAX_SOURCE_BYTES { return Err((ScriptFault::Error, format!("the script is longer than {} KiB", MAX_SOURCE_BYTES / 1024))); }
        let fail = |e: mlua::Error| (if is_memory_error(&e) { ScriptFault::Memory } else { ScriptFault::Error }, error_text(&e));
        let env = self.lua.create_table().map_err(fail)?;
        let meta = self.lua.create_table().map_err(fail)?;
        meta.raw_set("__index", globals.clone()).map_err(fail)?;
        meta.set_readonly(true);
        env.set_metatable(Some(meta)).map_err(fail)?;
        env.set_safeenv(true);
        let name: String = src.name.chars().filter(|c| !c.is_control()).take(48).collect();
        self.shared.used.store(0, Relaxed);
        self.shared.start();
        // Text mode first: the compiler only ever sees text, and a source that is bytecode is refused.
        let result = self.lua.load(src.source.as_bytes()).set_name(format!("={name}")).set_mode(ChunkMode::Text).set_compiler(script_compiler()).set_environment(env.clone()).eval::<Value>();
        let flags = self.shared.flags.load(Relaxed);
        self.shared.used.store(0, Relaxed);
        let value = match result {
            Ok(v) => { if let Some(f) = classify(flags, Ok(())) { return Err((f, "the script was stopped while loading".into())); } v }
            Err(e) => return Err((classify(flags, Err(&e)).unwrap_or(ScriptFault::Error), error_text(&e))),
        };
        let tick = match value {
            Value::Function(f) => f,
            _ => match env.raw_get::<Value>("tick") {
                Ok(Value::Function(f)) => f,
                _ => return Err((ScriptFault::Error, "the script must return its tick function or define a global function `tick`".into())),
            },
        };
        Ok(Loaded { tick, outputs: src.outputs.iter().map(|o| o.1).collect() })
    }
}

/// Bytes a linear-time builtin may produce or scan per work unit charged to the budget.
pub const BYTES_PER_UNIT: u64 = 128;

/// What a call of a wrapped builtin will touch, in bytes, read from its arguments before it runs.
type Cost = fn(&MultiValue) -> u64;

/// Builtins whose work grows with an argument rather than with the interrupts it raises: each call charges
/// `cost / BYTES_PER_UNIT` work units to the interrupt budget before it runs, so a size that the budget
/// cannot pay for faults with `Budget` instead of running for milliseconds without an interrupt (the
/// "constant-time or chunked" rule of L369, applied to the standard library).
const CHARGED_BUILTINS: [(&str, &str, Cost); 19] = [
    ("string", "rep", |a| { let n = count(a.get(1)); str_len(a.front()).saturating_mul(n).saturating_add(str_len(a.get(2)).saturating_mul(n.saturating_sub(1))) }),
    ("string", "upper", |a| str_len(a.front())),
    ("string", "lower", |a| str_len(a.front())),
    ("string", "reverse", |a| str_len(a.front())),
    ("string", "sub", |a| sub_len(str_len(a.front()), a.get(1), a.get(2))),
    ("string", "find", |a| str_len(a.front())),
    ("string", "split", |a| str_len(a.front())),
    ("string", "byte", |a| sub_len(str_len(a.front()), a.get(1), a.get(2).or(a.get(1)))),
    ("table", "create", |a| count(a.front()).saturating_mul(16)),
    ("table", "concat", |a| span(a.front(), a.get(2), a.get(3)).saturating_mul(16)),
    ("table", "move", |a| range_count(a.get(1), a.get(2)).saturating_mul(16)),
    ("table", "unpack", |a| span(a.front(), a.get(1), a.get(2)).saturating_mul(16)),
    ("buffer", "create", |a| count(a.front())),
    ("buffer", "fill", |a| a.get(3).map_or_else(|| buf_len(a.front()), |c| count(Some(c)))),
    ("buffer", "copy", |a| a.get(4).map_or_else(|| buf_len(a.get(2)), |c| count(Some(c)))),
    ("buffer", "tostring", |a| buf_len(a.front())),
    ("buffer", "fromstring", |a| str_len(a.front())),
    ("buffer", "readstring", |a| count(a.get(2))),
    ("buffer", "writestring", |a| a.get(3).map_or_else(|| str_len(a.get(2)), |c| count(Some(c)))),
];

fn str_len(v: Option<&Value>) -> u64 { match v { Some(Value::String(s)) => s.as_bytes().len() as u64, Some(Value::Number(_) | Value::Integer(_)) => 24, _ => 0 } }
fn buf_len(v: Option<&Value>) -> u64 { match v { Some(Value::Buffer(b)) => b.len() as u64, _ => 0 } }
/// A count argument: non-numbers and negatives are 0, huge values saturate.
fn count(v: Option<&Value>) -> u64 { v.and_then(number).filter(|x| *x > 0.0).map_or(0, |x| if x >= u64::MAX as f64 { u64::MAX } else { x as u64 }) }
fn range_count(f: Option<&Value>, e: Option<&Value>) -> u64 {
    match (f.and_then(number), e.and_then(number)) { (Some(f), Some(e)) if e >= f => { let n = e - f + 1.0; if n >= u64::MAX as f64 { u64::MAX } else { n as u64 } } _ => 0 }
}
/// Elements from `i` (default 1) to `j` (default `#t`) of a table argument.
fn span(t: Option<&Value>, i: Option<&Value>, j: Option<&Value>) -> u64 {
    let len = match t { Some(Value::Table(t)) => t.raw_len() as f64, _ => 0.0 };
    let i = i.and_then(number).unwrap_or(1.0);
    let j = j.and_then(number).unwrap_or(len);
    range_count(Some(&Value::Number(i)), Some(&Value::Number(j)))
}
/// Length of `s:sub(i, j)` under Lua's index rules.
fn sub_len(len: u64, i: Option<&Value>, j: Option<&Value>) -> u64 {
    let l = len as f64;
    let norm = |x: f64| if x < 0.0 { (l + x + 1.0).max(0.0) } else { x };
    let i = norm(i.and_then(number).unwrap_or(1.0)).max(1.0);
    let j = norm(j.and_then(number).unwrap_or(-1.0)).min(l);
    if j >= i { (j - i + 1.0) as u64 } else { 0 }
}

/// The Luau side of a charged builtin: the Rust `charge` only reads the arguments and pays, then Luau calls
/// the original builtin itself. An argument error is then a native Luau error; raised through a Rust
/// callback it would cost several hundred interrupts' worth of time, and grows with each one a script
/// catches and drops (mlua's error path), so `pcall(string.rep)` in a loop ran 0.8 s on one budget.
const CHARGED_WRAPPER: &str = "local orig, charge = ... return function(...) charge(...) return orig(...) end";

fn charged_builtin(lua: &Lua, shared: &std::sync::Arc<Shared>, orig: Function, cost: Cost) -> mlua::Result<Function> {
    let sh = shared.clone();
    let charge = lua.create_function(move |_, args: MultiValue| {
        let bytes = cost(&args);
        let units = (bytes / BYTES_PER_UNIT).min(u32::MAX as u64) as u32;
        if units > 0 {
            sh.charge(units)?;
            // The output this pays for is not charged again as memory growth at the next interrupt.
            let m = sh.mem_mark.load(Relaxed);
            if m != usize::MAX { sh.mem_mark.store(m.saturating_add(bytes.min(usize::MAX as u64) as usize), Relaxed); }
        }
        Ok(())
    })?;
    lua.load(CHARGED_WRAPPER).set_name("=host").set_mode(ChunkMode::Text).call::<Function>((orig, charge))
}

/// Work units charged each time `pcall`, `xpcall` or `coroutine.resume` returns a caught error. Luau
/// raises errors as C++ exceptions, and one raise and catch costs about as much as 80 interrupts of plain
/// code (4.2 us natively on Apple Silicon against the 50 ns per interrupt of the L375 gate); uncharged, a
/// `pcall(error)` loop ran 21 ms on one budget, ten times the 2 ms backstop.
pub const CAUGHT_ERROR_UNITS: u32 = 100;

/// Host guards, run after the charged builtins are installed and before the sandbox: every place a script
/// can catch an error charges [`CAUGHT_ERROR_UNITS`] when it reports one. They are Luau functions, so a
/// yield inside `pcall` still works; the Rust `caught` only runs after the protected call has returned.
const CATCH_GUARDS: &str = r##"
local caught = ...
local rawpcall, rawxpcall, rawresume = pcall, xpcall, coroutine.resume
local function settle(ok, ...)
    if not ok then caught() end
    return ok, ...
end
pcall = function(...) return settle(rawpcall(...)) end
xpcall = function(...) return settle(rawxpcall(...)) end
coroutine.resume = function(...) return settle(rawresume(...)) end
"##;

/// One registry function as a Lua global.
fn host_function(lua: &Lua, shared: &std::sync::Arc<Shared>, index: u16) -> mlua::Result<Function> {
    let sh = shared.clone();
    lua.create_function(move |lua, args: MultiValue| {
        let f = &host_api()[index as usize];
        sh.charge(f.work_units)?;
        if sh.past_backstop() { sh.flags.fetch_or(F_BACKSTOP, Relaxed); return Err(stop_error(F_BACKSTOP)); }
        let bad = |what: &str| { sh.flags.fetch_or(F_HOST, Relaxed); Err(mlua::Error::runtime(format!("{}: {what}", f.name))) };
        if args.len() != f.args.len() { return bad(&format!("takes {} argument(s), got {}", f.args.len(), args.len())); }
        let mut conv: [PortValue; 3] = Default::default();
        for (i, (v, t)) in args.iter().zip(f.args).enumerate() {
            match host_arg(v, *t) { Some(p) => conv[i] = p, None => return bad(&format!("argument {} has the wrong type", i + 1)) }
        }
        let ApiSlot(Some(api)) = sh.api() else { return bad("host functions can only be called while the script runs") };
        let args = &conv[..f.args.len()];
        // SAFETY: see `ApiSlot`; the pointer is live for the whole of `LuauHost::run`, which is on the stack.
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { (*api).call(index, args) }));
        match out {
            Ok(Ok(v)) => to_lua(lua, &crate::graph::library::sanitize(v), None),
            Ok(Err(_)) => bad("invalid call"),
            Err(_) => bad("host failure"),
        }
    })
}

/// A host function argument as the registry's declared type; `None` for a value of the wrong kind.
fn host_arg(v: &Value, ty: PortType) -> Option<PortValue> {
    use PortType as T;
    Some(match (v, ty) {
        (Value::Nil, _) => PortValue::None,
        (Value::Number(x), T::Scalar | T::Angle) => PortValue::Scalar(*x),
        (Value::Integer(x), T::Scalar | T::Angle) => PortValue::Scalar(*x as f64),
        (Value::Boolean(b), T::Scalar | T::Angle | T::Bool) => PortValue::Bool(*b),
        (Value::Vector(_) | Value::Table(_), T::Vec3) => PortValue::Vec3(vec_from(v)?),
        (Value::Number(_) | Value::Integer(_), T::Track) => track_from(v).map(PortValue::Track).unwrap_or(PortValue::None),
        (Value::String(s), T::Stance) => PortValue::Stance(stance_from(&s.to_string_lossy())?),
        (Value::String(s), T::Emcon) => PortValue::Emcon(emcon_from(&s.to_string_lossy())?),
        (Value::String(s), T::Text) => PortValue::Text(Arc::from(clip_text(&s.to_string_lossy()))),
        _ => return None,
    })
}

fn number(v: &Value) -> Option<f64> {
    match v { Value::Number(x) => Some(*x), Value::Integer(i) => Some(*i as f64), _ => None }
}

fn vec_from(v: &Value) -> Option<DVec3> {
    match v {
        Value::Vector(v) => Some(DVec3::new(v.x() as f64, v.y() as f64, v.z() as f64)),
        Value::Table(t) => {
            let get = |k: &str, i: i64| -> Option<f64> {
                number(&t.raw_get::<Value>(k).ok()?).or_else(|| number(&t.raw_get::<Value>(i).ok()?))
            };
            Some(DVec3::new(get("x", 1)?, get("y", 2)?, get("z", 3)?))
        }
        _ => None,
    }
}

fn track_from(v: &Value) -> Option<TrackId> {
    let x = number(v)?;
    (x.fract() == 0.0 && x >= 1.0 && x <= u32::MAX as f64).then_some(TrackId(x as u32))
}

fn index_from(v: &Value, n: usize) -> Option<usize> {
    let x = number(v)?;
    (x.fract() == 0.0 && x >= 0.0 && x < n as f64).then_some(x as usize)
}

const STANCES: [(Stance, &str); 4] = [(Stance::Friend, "friend"), (Stance::Neutral, "neutral"), (Stance::Suspect, "suspect"), (Stance::Hostile, "hostile")];
const EMCONS: [(Emcon, &str); 3] = [(Emcon::Silent, "silent"), (Emcon::Burst, "burst"), (Emcon::Active, "active")];

fn stance_from(s: &str) -> Option<Stance> { STANCES.iter().find(|(_, n)| n.eq_ignore_ascii_case(s.trim())).map(|(x, _)| *x) }
fn emcon_from(s: &str) -> Option<Emcon> { EMCONS.iter().find(|(_, n)| n.eq_ignore_ascii_case(s.trim())).map(|(x, _)| *x) }
fn stance_name(s: Stance) -> &'static str { STANCES.iter().find(|(x, _)| *x == s).map_or("suspect", |(_, n)| n) }
fn emcon_name(e: Emcon) -> &'static str { EMCONS.iter().find(|(x, _)| *x == e).map_or("burst", |(_, n)| n) }

fn clip_text(s: &str) -> &str { &s[..floor_char_boundary(s, MAX_TEXT_BYTES)] }

fn sensor_kind_name(k: content::part::SensorKind) -> &'static str {
    use content::part::SensorKind as K;
    match k {
        K::SearchRadar => "search_radar", K::NavRadar => "nav_radar", K::FireControlRadar => "fire_control_radar", K::Seeker => "seeker",
        K::Esm => "esm", K::PassiveSonar => "passive_sonar", K::ActiveSonar => "active_sonar", K::Visual => "visual", K::Camera => "camera",
    }
}

fn domain_index(d: Domain) -> f64 { match d { Domain::Surface => 0.0, Domain::Subsurface => 1.0, Domain::Air => 2.0, Domain::Weapon => 3.0 } }

/// Detection tables handed to the script this run, by table address, for DetectionList pass-through. The
/// table handle keeps each one alive for the run, so its address cannot be reused by another table.
type DetectionMap = Vec<(usize, Detection, Table)>;

/// A run-time value as a Lua value. `dets` collects DetectionList tables for pass-through (inputs only).
fn to_lua(lua: &Lua, v: &PortValue, dets: Option<&mut DetectionMap>) -> mlua::Result<Value> {
    Ok(match v {
        PortValue::None => Value::Nil,
        PortValue::Bool(b) => Value::Boolean(*b),
        PortValue::Scalar(x) => if x.is_finite() { Value::Number(*x) } else { Value::Nil },
        PortValue::Vec3(p) => if p.is_finite() { Value::Vector(mlua::Vector::new(p.x as f32, p.y as f32, p.z as f32)) } else { Value::Nil },
        PortValue::Text(s) => Value::String(lua.create_string(s.as_bytes())?),
        PortValue::Stance(s) => Value::String(lua.create_string(stance_name(*s))?),
        PortValue::Emcon(e) => Value::String(lua.create_string(emcon_name(*e))?),
        PortValue::Preset(p) => Value::Number(*p as u8 as f64),
        PortValue::Rung(r) => Value::Number(*r as u8 as f64),
        PortValue::Track(t) => if t.is_none() { Value::Nil } else { Value::Number(t.0 as f64) },
        PortValue::TrackList(list) => {
            let n = list.len().min(MAX_LIST);
            let t = lua.create_table_with_capacity(n, 0)?;
            for (i, id) in list.iter().take(n).enumerate() { t.raw_set(i + 1, id.0 as f64)?; }
            Value::Table(t)
        }
        PortValue::Detections(list) => {
            let n = list.len().min(MAX_LIST);
            let t = lua.create_table_with_capacity(n, 0)?;
            let mut map = dets;
            for (i, d) in list.iter().take(n).enumerate() {
                let e = detection_table(lua, d)?;
                if let Some(m) = map.as_deref_mut() { m.push((e.to_pointer() as usize, *d, e.clone())); }
                t.raw_set(i + 1, e)?;
            }
            Value::Table(t)
        }
        PortValue::Part(slot) => Value::Number(slot.0 as f64),
        PortValue::Orders => Value::Boolean(true),
    })
}

/// A detection as a script sees it: the measurement and its errors, never the truth fields.
fn detection_table(lua: &Lua, d: &Detection) -> mlua::Result<Table> {
    let t = lua.create_table_with_capacity(0, 8)?;
    if d.sensor.index != crate::ids::SensorRef::SEEKER { t.raw_set("sensor", d.sensor.index as f64)?; }
    t.raw_set("kind", sensor_kind_name(d.kind))?;
    match d.measurement {
        Measurement::RangeBearing { range_m, bearing_rad, height_m } => {
            t.raw_set("bearing", bearing_rad)?;
            t.raw_set("range_m", range_m)?;
            if let Some(h) = height_m { t.raw_set("height_m", h)?; }
        }
        Measurement::BearingOnly { bearing_rad } => t.raw_set("bearing", bearing_rad)?,
    }
    t.raw_set("sigma_range_m", d.sigma_range_m)?;
    t.raw_set("sigma_bearing", d.sigma_bearing_rad)?;
    if let Some(dom) = d.domain_hint { t.raw_set("domain", domain_index(dom))?; }
    Ok(t)
}

/// A returned Lua value as the output port's type; anything that does not fit is `None`.
fn from_lua(v: &Value, ty: PortType, dets: &DetectionMap) -> PortValue {
    use PortType as T;
    match (v, ty) {
        (Value::Nil, _) => PortValue::None,
        (Value::Boolean(b), T::Bool | T::Scalar | T::Angle) => PortValue::Bool(*b),
        (Value::Number(_) | Value::Integer(_), T::Bool | T::Scalar | T::Angle) => number(v).map_or(PortValue::None, PortValue::Scalar),
        (Value::Vector(_) | Value::Table(_), T::Vec3) => vec_from(v).map_or(PortValue::None, PortValue::Vec3),
        (Value::String(s), T::Text) => PortValue::Text(Arc::from(clip_text(&s.to_string_lossy()))),
        (Value::String(s), T::Stance) => stance_from(&s.to_string_lossy()).map_or(PortValue::None, PortValue::Stance),
        (Value::Number(_) | Value::Integer(_), T::Stance) => index_from(v, STANCES.len()).map_or(PortValue::None, |i| PortValue::Stance(STANCES[i].0)),
        (Value::String(s), T::Emcon) => emcon_from(&s.to_string_lossy()).map_or(PortValue::None, PortValue::Emcon),
        (Value::Number(_) | Value::Integer(_), T::Emcon) => index_from(v, EMCONS.len()).map_or(PortValue::None, |i| PortValue::Emcon(EMCONS[i].0)),
        (Value::Number(_) | Value::Integer(_), T::Track) => track_from(v).map_or(PortValue::None, PortValue::Track),
        (Value::Table(t), T::TrackList) => {
            let n = t.raw_len().min(MAX_LIST);
            let ids: Vec<TrackId> = (1..=n).filter_map(|i| track_from(&t.raw_get::<Value>(i).ok()?)).collect();
            PortValue::TrackList(Arc::new(ids))
        }
        (Value::Table(t), T::DetectionList) => {
            let n = t.raw_len().min(MAX_LIST);
            let list: Vec<Detection> = (1..=n).filter_map(|i| match t.raw_get::<Value>(i).ok()? {
                Value::Table(e) => { let p = e.to_pointer() as usize; dets.binary_search_by_key(&p, |d| d.0).ok().map(|k| dets[k].1) }
                _ => None,
            }).collect();
            PortValue::Detections(Arc::new(list))
        }
        (Value::Number(_) | Value::Integer(_), T::PartState) => index_from(v, u16::MAX as usize + 1).map_or(PortValue::None, |i| PortValue::Part(PartSlot(i as u16))),
        (v, T::Orders) => if matches!(v, Value::Boolean(false)) { PortValue::None } else { PortValue::Orders },
        _ => PortValue::None,
    }
}

impl ScriptHost for LuauHost {
    fn load(&mut self, vessel: EntityId, sources: &[ScriptSource], limits: ScriptLimits) -> Vec<Result<(), ScriptFault>> {
        self.vms.remove(&vessel);
        if !sources.iter().any(|s| s.enabled) { return sources.iter().map(|_| Ok(())).collect(); }
        let mut vm = match Vm::new(vessel, limits) {
            Ok(vm) => vm,
            Err(_) => return sources.iter().map(|s| if s.enabled { Err(ScriptFault::Error) } else { Ok(()) }).collect(),
        };
        // The original globals: read-only now, the fallback of every script's own environment.
        let globals = vm.lua.globals();
        let globals = match globals.metatable().and_then(|m| m.raw_get::<Table>("__index").ok()) { Some(g) => g, None => globals };
        let mut out = Vec::with_capacity(sources.len());
        for src in sources {
            if !src.enabled { vm.scripts.push(None); vm.errors.push(None); out.push(Ok(())); continue; }
            match vm.load_one(&globals, src) {
                Ok(l) => { vm.scripts.push(Some(l)); vm.errors.push(None); out.push(Ok(())); }
                Err((f, msg)) => { vm.scripts.push(None); vm.errors.push(Some(msg)); out.push(Err(f)); }
            }
        }
        self.vms.insert(vessel, vm);
        out
    }

    fn begin_tick(&mut self, vessel: EntityId) {
        if let Some(vm) = self.vms.get(&vessel) { vm.shared.used.store(0, Relaxed); vm.shared.flags.store(0, Relaxed); }
    }

    fn run(&mut self, vessel: EntityId, index: u16, inputs: &[PortValue], api: &mut dyn HostApi, out: &mut [PortValue]) -> ScriptRun {
        let error = ScriptRun { fault: Some(ScriptFault::Error), interrupts: 0 };
        let Some(vm) = self.vms.get_mut(&vessel) else { return error };
        let Some(Some(script)) = vm.scripts.get(index as usize) else { return error };
        let sh = vm.shared.clone();
        let before = sh.used.load(Relaxed);
        sh.start();
        let mut dets: DetectionMap = Vec::new();
        let mut args = Vec::with_capacity(inputs.len());
        for v in inputs {
            match to_lua(&vm.lua, v, Some(&mut dets)) {
                Ok(x) => args.push(x),
                Err(e) => {
                    let fault = if is_memory_error(&e) { ScriptFault::Memory } else { ScriptFault::Error };
                    if let Some(slot) = vm.errors.get_mut(index as usize) { *slot = Some(error_text(&e)); }
                    return ScriptRun { fault: Some(fault), interrupts: 0 };
                }
            }
        }
        dets.sort_unstable_by_key(|d| d.0);
        // SAFETY: the 'static is a lie told only to the slot; `ApiGuard` clears it before `api` goes away.
        let ptr: *mut (dyn HostApi + '_) = api;
        let ptr: *mut (dyn HostApi + 'static) = unsafe { std::mem::transmute(ptr) };
        sh.set_api(ApiSlot(Some(ptr)));
        let result = {
            let _guard = ApiGuard(&sh);
            script.tick.call::<MultiValue>(MultiValue::from_vec(args))
        };
        let flags = sh.flags.load(Relaxed);
        let budget = sh.budget.load(Relaxed);
        let interrupts = sh.used.load(Relaxed).saturating_sub(before).min(budget.saturating_add(1).saturating_sub(before));
        let fault = classify(flags, result.as_ref().map(|_| ()));
        match (fault, result) {
            (None, Ok(values)) => {
                for (slot, (v, ty)) in out.iter_mut().zip(values.iter().chain(std::iter::repeat(&Value::Nil)).zip(&script.outputs)) {
                    *slot = from_lua(v, *ty, &dets);
                }
                ScriptRun { fault: None, interrupts }
            }
            (f, r) => {
                let msg = match (&r, f) {
                    (Err(e), _) => error_text(e),
                    (Ok(_), Some(ScriptFault::Budget)) => "the interrupt budget ran out".into(),
                    (Ok(_), Some(ScriptFault::Backstop)) => "the wall-clock backstop stopped the script".into(),
                    _ => "a host function was called with the wrong arguments".into(),
                };
                if let Some(slot) = vm.errors.get_mut(index as usize) { *slot = Some(msg); }
                ScriptRun { fault: Some(f.unwrap_or(ScriptFault::Error)), interrupts }
            }
        }
    }

    fn unload(&mut self, vessel: EntityId) { self.vms.remove(&vessel); }

    fn memory_bytes(&self, vessel: EntityId) -> usize { self.vms.get(&vessel).map_or(0, |vm| vm.lua.used_memory()) }
}

/// The fuzz harness of L2127-2138, shared by `fuzz/fuzz_targets/{host_api,script_source}.rs` and the
/// regression tests that `cargo test` runs on every platform. Every VM is a production [`LuauHost`] VM,
/// set up in the production order; the oracles are plain functions so a crash input becomes a test.
pub mod fuzz {
    use super::*;
    use crate::script::{check_args, host_fn as h, intercept_point};

    /// The vessel a fuzz script runs on, and the second vessel used for the isolation oracle.
    pub const VESSEL: EntityId = EntityId { index: 7, generation: 1 };
    pub const OTHER: EntityId = EntityId { index: 8, generation: 1 };
    /// Graph ticks each input runs for.
    pub const TICKS: u32 = 3;
    /// Fixed allowance over the memory limit (allocator rounding and mlua's own bookkeeping, L2132).
    pub const MEMORY_SLACK: usize = 64 * 1024;
    /// The fuzz backstop. Sanitizer builds run several times slower than release, so the production 2 ms
    /// would trip on honest scripts; at 100 ms a trip means the budget failed to stop a runaway script,
    /// which is itself an oracle failure ("stopped only by the wall-clock backstop", L2144).
    pub const FUZZ_BACKSTOP_MICROS: u32 = 100_000;
    /// Inputs of the fuzz script node and the types its outputs are read as.
    pub const INPUTS: [PortType; 3] = [PortType::Scalar, PortType::Vec3, PortType::TrackList];
    pub const OUTPUTS: [PortType; 8] = [PortType::Scalar, PortType::Vec3, PortType::Text, PortType::Track, PortType::TrackList, PortType::Bool, PortType::Stance, PortType::DetectionList];

    /// Production limits with the fuzz backstop.
    pub fn limits() -> ScriptLimits { ScriptLimits { backstop_micros: FUZZ_BACKSTOP_MICROS, ..ScriptLimits::default() } }

    /// A [`HostApi`] over a fixed synthetic ship view that follows every rule of the registry (see
    /// `script::host_api`) and counts the work it does, in host work units, against each function's
    /// declared bound.
    #[derive(Clone, Debug)]
    pub struct FixtureApi {
        pub tick: u32,
        pub calls: u64,
        /// The first call that did more work than its function declares: (function, work done).
        pub over_bound: Option<(u16, u32)>,
        tracks: Vec<(TrackId, DVec3, DVec3, Stance)>,
        detections: Vec<(Option<u16>, f64, Option<f64>)>,
    }

    impl Default for FixtureApi {
        fn default() -> Self {
            Self {
                tick: 0, calls: 0, over_bound: None,
                tracks: vec![
                    (TrackId(2), DVec3::new(3_000.0, 0.0, -4_000.0), DVec3::new(-5.0, 0.0, 2.0), Stance::Hostile),
                    (TrackId(3), DVec3::new(-1_200.0, 0.0, 800.0), DVec3::ZERO, Stance::Neutral),
                    (TrackId(5), DVec3::new(9_000.0, 300.0, 0.0), DVec3::new(-240.0, 0.0, 0.0), Stance::Hostile),
                    (TrackId(8), DVec3::new(0.0, 0.0, -15_000.0), DVec3::new(0.0, 0.0, 6.0), Stance::Suspect),
                    (TrackId(13), DVec3::new(500.0, -40.0, 500.0), DVec3::new(1.0, 0.0, 1.0), Stance::Friend),
                ],
                detections: vec![(Some(4), 0.6, Some(5_000.0)), (Some(6), 2.1, None), (None, 1.2, Some(800.0))],
            }
        }
    }

    const OWN_POS: DVec3 = DVec3::new(100.0, 0.0, -200.0);

    fn idx(v: &PortValue, n: usize) -> Option<usize> {
        match v { PortValue::Scalar(x) if x.is_finite() && *x >= 0.0 && x.trunc() < n as f64 => Some(x.trunc() as usize), PortValue::Bool(b) if (*b as usize) < n => Some(*b as usize), _ => None }
    }
    fn num(v: &PortValue) -> Option<f64> { match v { PortValue::Scalar(x) if x.is_finite() => Some(*x), PortValue::Bool(b) => Some(*b as u8 as f64), _ => None } }
    fn vec(v: &PortValue) -> Option<DVec3> { match v { PortValue::Vec3(p) if p.is_finite() => Some(*p), _ => None } }
    fn s(x: f64) -> PortValue { if x.is_finite() { PortValue::Scalar(x) } else { PortValue::None } }
    fn compass(from: DVec3, to: DVec3) -> Option<f64> {
        let d = to - from;
        if d.x == 0.0 && d.z == 0.0 { return None; }
        let b = libm::atan2(d.x, -d.z);
        Some(if b < 0.0 { b + core::f64::consts::TAU } else { b })
    }

    impl HostApi for FixtureApi {
        fn call(&mut self, index: u16, args: &[PortValue]) -> Result<PortValue, ScriptFault> {
            let f = check_args(index, args)?;
            self.calls += 1;
            let a = |i: usize| args.get(i).cloned().unwrap_or_default();
            let part = |v: &PortValue| idx(v, 5);
            let track = |v: &PortValue| idx(v, self.tracks.len()).map(|i| self.tracks[i]);
            let det = |v: &PortValue| idx(v, self.detections.len()).map(|i| self.detections[i]);
            let mut work = 1;
            let out = match index {
                h::OWN_POSITION => PortValue::Vec3(OWN_POS),
                h::OWN_VELOCITY => PortValue::Vec3(DVec3::new(3.0, 0.0, -5.0)),
                h::OWN_HEADING => s(0.54),
                h::OWN_SPEED_KN => s(11.3),
                h::OWN_HEEL => s(-0.02),
                h::SORTIE_TIME_S => s(self.tick as f64 / 30.0),
                h::HULL_HEALTH => s(0.875),
                h::PART_HEALTH => part(&a(0)).map_or(PortValue::None, |p| s(1.0 - p as f64 * 0.2)),
                h::PART_WORKS => part(&a(0)).map_or(PortValue::None, |p| PortValue::Bool(p != 3)),
                h::PART_AMMO_FRACTION => { work = 2; part(&a(0)).filter(|p| *p < 2).map_or(PortValue::None, |p| s(0.5 + p as f64 * 0.25)) }
                h::PART_TRAIN => part(&a(0)).filter(|p| *p < 3).map_or(PortValue::None, |p| s(p as f64 * 0.3)),
                h::PART_ELEVATION => part(&a(0)).filter(|p| *p < 2).map_or(PortValue::None, |_| s(0.1)),
                h::TRACK_COUNT => s(self.tracks.len() as f64),
                h::TRACK_ID => track(&a(0)).map_or(PortValue::None, |t| PortValue::Track(t.0)),
                h::TRACK_POSITION => track(&a(0)).map_or(PortValue::None, |t| PortValue::Vec3(t.1 + t.2 * (self.tick as f64 / 30.0))),
                h::TRACK_VELOCITY => track(&a(0)).map_or(PortValue::None, |t| PortValue::Vec3(t.2)),
                h::TRACK_STANCE => track(&a(0)).map_or(PortValue::None, |t| PortValue::Stance(t.3)),
                h::TRACK_DOMAIN => track(&a(0)).map_or(PortValue::None, |t| s(if t.1.y > 100.0 { 2.0 } else if t.1.y < 0.0 { 1.0 } else { 0.0 })),
                h::TRACK_GRADE => track(&a(0)).map_or(PortValue::None, |t| s((t.0 .0 % 4) as f64)),
                h::TRACK_RUNG => track(&a(0)).map_or(PortValue::None, |t| s((t.0 .0 % 5) as f64)),
                h::TRACK_RADIUS95_M => { work = 2; track(&a(0)).map_or(PortValue::None, |t| s(50.0 + t.2.length() * 10.0)) }
                h::TRACK_INBOUND => track(&a(0)).map_or(PortValue::None, |t| PortValue::Bool((OWN_POS - t.1).dot(t.2) > 0.0)),
                h::DETECTION_COUNT => s(self.detections.len() as f64),
                h::DETECTION_SENSOR => det(&a(0)).and_then(|d| d.0).map_or(PortValue::None, |x| s(x as f64)),
                h::DETECTION_BEARING => det(&a(0)).map_or(PortValue::None, |d| s(d.1)),
                h::DETECTION_RANGE_M => det(&a(0)).and_then(|d| d.2).map_or(PortValue::None, s),
                h::DESIGNATED_TRACK => PortValue::Track(TrackId(5)),
                h::EMCON => PortValue::Emcon(Emcon::Burst),
                h::BEARING_TO => { work = 2; vec(&a(0)).and_then(|p| compass(OWN_POS, p)).map_or(PortValue::None, s) }
                h::RANGE_TO => vec(&a(0)).map_or(PortValue::None, |p| s(DVec2::new(p.x - OWN_POS.x, p.z - OWN_POS.z).length())),
                h::WRAP_ANGLE => num(&a(0)).filter(|x| x.abs() < 1e9).map_or(PortValue::None, |x| {
                    let w = x - core::f64::consts::TAU * libm::floor((x + core::f64::consts::PI) / core::f64::consts::TAU);
                    s(if w >= core::f64::consts::PI { w - core::f64::consts::TAU } else { w })
                }),
                h::CLAMP => match (num(&a(0)), num(&a(1)), num(&a(2))) { (Some(x), Some(lo), Some(hi)) if lo <= hi => s(x.clamp(lo, hi)), _ => PortValue::None },
                h::INTERCEPT_POINT => { work = 4; match (vec(&a(0)), vec(&a(1)), num(&a(2))) { (Some(p), Some(v), Some(sp)) => intercept_point(OWN_POS, p, v, sp).map_or(PortValue::None, PortValue::Vec3), _ => PortValue::None } }
                h::TRACK_INDEX => match a(0) {
                    PortValue::Track(t) => {
                        let (mut lo, mut hi, mut found) = (0usize, self.tracks.len(), None);
                        work = 0;
                        while lo < hi {
                            work += 1;
                            let mid = (lo + hi) / 2;
                            match self.tracks[mid].0.cmp(&t) { core::cmp::Ordering::Less => lo = mid + 1, core::cmp::Ordering::Greater => hi = mid, core::cmp::Ordering::Equal => { found = Some(mid); break } }
                        }
                        work = work.max(1);
                        found.map_or(PortValue::None, |i| s(i as f64))
                    }
                    _ => PortValue::None,
                },
                _ => PortValue::None,
            };
            if work > f.work_units && self.over_bound.is_none() { self.over_bound = Some((index, work)); }
            Ok(out)
        }
    }

    /// One argument of a generated host call, edge values included (L2129).
    #[derive(Clone, Debug, PartialEq)]
    pub enum FuzzArg {
        Nil,
        Bool(bool),
        Num(f64),
        Nan,
        Inf,
        NegInf,
        /// 2^53, past exact integers.
        Huge,
        /// A huge negative count.
        NegCount,
        Int(i32),
        Vector(f32, f32, f32),
        /// Tables nested this deep (capped at 150).
        Deep(u8),
        /// A table that contains itself.
        Cyclic,
        /// A string of this many bytes, built at run time.
        Long(u32),
        Text(String),
        Function,
    }

    /// One generated host call: registry function `func` (an index past the registry calls a missing
    /// global), its arguments, whether it runs inside `pcall`, and how many times in a loop.
    #[derive(Clone, Debug, PartialEq)]
    pub struct FuzzCall { pub func: u16, pub args: Vec<FuzzArg>, pub protected: bool, pub repeat: u16 }

    fn lua_number(x: f64) -> String {
        if x.is_nan() { "(0/0)".into() } else if x == f64::INFINITY { "math.huge".into() } else if x == f64::NEG_INFINITY { "(-math.huge)".into() } else { format!("({x:?})") }
    }

    fn lua_string(s: &str) -> String {
        let mut o = String::with_capacity(s.len() + 2);
        o.push('"');
        for b in s.bytes().take(4_096) {
            match b { b'"' | b'\\' => { o.push('\\'); o.push(b as char); } 0x20..=0x7e => o.push(b as char), _ => o.push_str(&format!("\\{b}")) }
        }
        o.push('"');
        o
    }

    fn render_arg(a: &FuzzArg) -> String {
        match a {
            FuzzArg::Nil => "nil".into(),
            FuzzArg::Bool(b) => b.to_string(),
            FuzzArg::Num(x) => lua_number(*x),
            FuzzArg::Nan => "(0/0)".into(),
            FuzzArg::Inf => "math.huge".into(),
            FuzzArg::NegInf => "(-math.huge)".into(),
            FuzzArg::Huge => "(2^53)".into(),
            FuzzArg::NegCount => "(-2^31)".into(),
            FuzzArg::Int(i) => format!("({i})"),
            FuzzArg::Vector(x, y, z) => format!("vector.create({}, {}, {})", lua_number(*x as f64), lua_number(*y as f64), lua_number(*z as f64)),
            FuzzArg::Deep(d) => { let d = (*d).min(150) as usize; format!("{}{}", "{".repeat(d + 1), "}".repeat(d + 1)) }
            FuzzArg::Cyclic => "(function() local t = {} t[1] = t t.x = t return t end)()".into(),
            FuzzArg::Long(n) => format!("string.rep(\"x\", {})", n.min(&(16 << 20))),
            FuzzArg::Text(s) => lua_string(s),
            FuzzArg::Function => "(function() return 1 end)".into(),
        }
    }

    /// Render a host-call sequence as a script node source: the tick function makes the calls and returns
    /// the last result through every output port, so each output conversion sees every kind of value.
    pub fn render_host_calls(calls: &[FuzzCall]) -> String {
        let mut src = String::from("local last = nil\nreturn function(a, b, c)\n");
        for c in calls.iter().take(256) {
            let name = host_api().get(c.func as usize).map_or_else(|| format!("no_such_host_fn_{}", c.func), |f| f.name.to_string());
            let args: Vec<String> = c.args.iter().take(8).map(render_arg).collect();
            let call = if c.protected {
                let mut v = vec![name];
                v.extend(args);
                format!("select(2, pcall({}))", v.join(", "))
            } else {
                format!("{name}({})", args.join(", "))
            };
            let n = c.repeat.clamp(1, 10_000);
            if n == 1 { src.push_str(&format!("    last = {call}\n")); } else { src.push_str(&format!("    for _ = 1, {n} do last = {call} end\n")); }
        }
        src.push_str("    return last, last, last, last, last, last, last, last\nend\n");
        src
    }

    /// What one tick of a fuzz run produced: fault, interrupts and outputs (in `Debug` form, so NaN compares
    /// equal to itself).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TickRecord { pub load: Option<ScriptFault>, pub fault: Option<ScriptFault>, pub interrupts: u32, pub outputs: String }

    /// The canary: a second script on the same vessel VM, run after the fuzz script every tick. It must find
    /// the removed globals still absent, the libraries intact and the shared global table unchanged.
    fn canary(known: &[String]) -> String {
        let mut s = String::from("local known = {");
        for k in known { s.push_str(&format!("[{}] = true, ", lua_string(k))); }
        s.push_str("}\nreturn function()\n");
        s.push_str("    if os ~= nil or debug ~= nil or require ~= nil or loadstring ~= nil or getfenv ~= nil or setfenv ~= nil or collectgarbage ~= nil or gcinfo ~= nil or newproxy ~= nil then return false end\n");
        s.push_str("    if math.floor(2.5) ~= 2 or string.rep(\"ab\", 2) ~= \"abab\" or tostring({}) ~= \"table\" then return false end\n");
        s.push_str("    for k in pairs(_G) do if not known[k] then return false end end\n");
        s.push_str("    return true\nend\n");
        s
    }

    fn source(name: &str, text: String, inputs: &[PortType], outputs: &[PortType]) -> ScriptSource {
        ScriptSource {
            name: name.into(), source: text, enabled: true,
            inputs: inputs.iter().enumerate().map(|(i, t)| (format!("in{i}"), *t)).collect(),
            outputs: outputs.iter().enumerate().map(|(i, t)| (format!("out{i}"), *t)).collect(),
        }
    }

    /// The global names a fresh vehicle VM has, sorted (for the canary).
    pub fn global_names() -> Vec<String> {
        let mut host = LuauHost::new();
        let src = source("names", "local n = {} for k in pairs(_G) do n[#n + 1] = k end table.sort(n) local s = table.concat(n, \",\") return function() return s end".into(), &[], &[PortType::Text]);
        host.load(VESSEL, &[src], limits());
        let mut out = [PortValue::None];
        host.begin_tick(VESSEL);
        host.run(VESSEL, 0, &[], &mut FixtureApi::default(), &mut out);
        match &out[0] { PortValue::Text(t) => t.split(',').map(str::to_string).collect(), _ => Vec::new() }
    }

    fn inputs(tick: u32) -> [PortValue; 3] {
        [PortValue::Scalar(tick as f64), PortValue::Vec3(DVec3::new(1.0, 2.0, 3.0)), PortValue::TrackList(Arc::new(vec![TrackId(2), TrackId(5)]))]
    }

    /// Run one input through a fresh host and check every per-tick oracle. Returns the output stream.
    fn run_once(text: &str, limits: ScriptLimits, known: &[String]) -> Result<Vec<TickRecord>, String> {
        let mut host = LuauHost::new();
        let loads = host.load(VESSEL, &[source("fuzz", text.to_string(), &INPUTS, &OUTPUTS), source("canary", canary(known), &[], &[PortType::Bool])], limits);
        if loads[1].is_err() { return Err(format!("the canary failed to load: {:?}", loads[1])); }
        let other = host.load(OTHER, &[source("canary", canary(known), &[], &[PortType::Bool])], limits);
        if other[0].is_err() { return Err("the other vessel's canary failed to load".into()); }
        let load = loads[0].err();
        if load == Some(ScriptFault::Backstop) { return Err("loading was stopped only by the wall-clock backstop".into()); }
        let mut stream = Vec::new();
        let mut api = FixtureApi::default();
        for tick in 0..TICKS {
            api.tick = tick;
            let (fault, interrupts, outputs) = if load.is_none() {
                host.begin_tick(VESSEL);
                let mut out: Vec<PortValue> = vec![PortValue::None; OUTPUTS.len()];
                let run = host.run(VESSEL, 0, &inputs(tick), &mut api, &mut out);
                let raw = host.vms.get(&VESSEL).map_or(0, |vm| vm.shared.used.load(Relaxed));
                if raw > limits.interrupts_per_tick.saturating_add(1) { return Err(format!("tick {tick}: {raw} work units charged past a budget of {}", limits.interrupts_per_tick)); }
                if run.interrupts > limits.interrupts_per_tick.saturating_add(1) { return Err(format!("tick {tick}: {} interrupts reported", run.interrupts)); }
                if run.fault == Some(ScriptFault::Backstop) { return Err(format!("tick {tick}: stopped only by the wall-clock backstop")); }
                (run.fault, run.interrupts, format!("{out:?}"))
            } else { (None, 0, String::new()) };
            let mem = host.memory_bytes(VESSEL);
            if mem > limits.memory_bytes + MEMORY_SLACK { return Err(format!("tick {tick}: {mem} bytes in use, limit {}", limits.memory_bytes)); }
            if let Some((f, w)) = api.over_bound { return Err(format!("host function {f} did {w} work units, over its declared bound")); }
            for v in [VESSEL, OTHER] {
                host.begin_tick(v);
                let mut ok = [PortValue::None];
                let index = if v == VESSEL { 1 } else { 0 };
                let r = host.run(v, index, &[], &mut FixtureApi::default(), &mut ok);
                if r.fault.is_some() || ok[0] != PortValue::Bool(true) { return Err(format!("tick {tick}: the canary on vessel {} saw a changed sandbox ({:?}, {:?})", v.index, r.fault, ok[0])); }
            }
            stream.push(TickRecord { load, fault, interrupts, outputs });
            if fault.is_some() || load.is_some() { break; }
        }
        Ok(stream)
    }

    /// Every oracle of L2132 on one script source: no panic, memory within the limit plus slack, the budget
    /// stops the script (never the backstop alone), the second run repeats the first exactly, no host function
    /// exceeds its bound, the removed globals stay unreachable, and nothing leaks between scripts or vessels.
    pub fn check_source_oracles(text: &str) -> Result<Vec<TickRecord>, String> {
        static KNOWN: OnceLock<Vec<String>> = OnceLock::new();
        let known = KNOWN.get_or_init(global_names);
        if known.len() < 20 { return Err(format!("could not list a fresh VM's globals ({} found)", known.len())); }
        let first = run_once(text, limits(), known)?;
        let second = run_once(text, limits(), known)?;
        if first != second { return Err(format!("two runs differ:\n{first:?}\n{second:?}")); }
        Ok(first)
    }

    /// The `script_source` target: raw bytes as Luau source (invalid UTF-8 is replaced, as the editor would).
    pub fn check_bytes(data: &[u8]) -> Result<Vec<TickRecord>, String> { check_source_oracles(&String::from_utf8_lossy(data)) }

    /// The `host_api` target: a generated call sequence.
    pub fn check_calls(calls: &[FuzzCall]) -> Result<Vec<TickRecord>, String> { check_source_oracles(&render_host_calls(calls)) }
}

#[cfg(test)]
mod tests {
    use super::fuzz::{self, FixtureApi, FuzzArg, FuzzCall};
    use super::*;
    use crate::script::host_fn as h;

    const V: EntityId = EntityId { index: 3, generation: 1 };

    /// Production limits with the wide fuzz backstop: these tests assert exact faults, and a preempted CI
    /// thread must not turn a budget or memory fault into a backstop trip. Only the backstop test is tight.
    fn lim() -> ScriptLimits { fuzz::limits() }

    fn src(text: &str, inputs: &[PortType], outputs: &[PortType]) -> ScriptSource {
        ScriptSource {
            name: "test".into(), source: text.into(), enabled: true,
            inputs: inputs.iter().map(|t| ("i".to_string(), *t)).collect(),
            outputs: outputs.iter().map(|t| ("o".to_string(), *t)).collect(),
        }
    }

    fn host_with(sources: &[ScriptSource], limits: ScriptLimits) -> (LuauHost, Vec<Result<(), ScriptFault>>) {
        let mut h = LuauHost::new();
        let r = h.load(V, sources, limits);
        (h, r)
    }

    fn run1(h: &mut LuauHost, index: u16, inputs: &[PortValue], n_out: usize) -> (ScriptRun, Vec<PortValue>) {
        h.begin_tick(V);
        let mut out = vec![PortValue::None; n_out];
        let r = h.run(V, index, inputs, &mut FixtureApi::default(), &mut out);
        (r, out)
    }

    /// Load one script with the given outputs and run it once.
    fn eval(text: &str, outputs: &[PortType]) -> (ScriptRun, Vec<PortValue>) {
        let (mut h, r) = host_with(&[src(text, &[], outputs)], lim());
        assert_eq!(r, vec![Ok(())], "{:?}", h.last_error(V, 0));
        run1(&mut h, 0, &[], outputs.len())
    }

    #[test]
    fn the_sandbox_removes_every_unsafe_global_and_freezes_the_libraries() {
        let (r, out) = eval(r#"return function()
            local gone = os == nil and debug == nil and require == nil and loadstring == nil and getfenv == nil
                and setfenv == nil and gcinfo == nil and collectgarbage == nil and newproxy == nil
            local frozen = not pcall(function() math.floor = nil end) and not pcall(function() string.x = 1 end)
                and not pcall(function() rawset(_G, "x", 1) end) and not pcall(function() setmetatable(string, {}) end)
            print("nothing happens")
            return gone, frozen, tostring({}), string.format("%s|%s|%d", {}, print, 3), tostring(nil)
        end"#, &[PortType::Bool, PortType::Bool, PortType::Text, PortType::Text, PortType::Text]);
        assert_eq!(r.fault, None);
        assert_eq!(out[0], PortValue::Bool(true), "os, debug, require, loadstring and friends are gone");
        assert_eq!(out[1], PortValue::Bool(true), "libraries and the globals are read-only");
        assert_eq!(out[2], PortValue::Text("table".into()), "no heap address");
        assert_eq!(out[3], PortValue::Text("table|function|3".into()));
        assert_eq!(out[4], PortValue::Text("nil".into()));
        // A protected metatable that claims a `__tostring` it does not have still prints no address.
        let (r, out) = eval(r#"return function()
            local t = setmetatable({}, { __metatable = { __tostring = true } })
            local u = setmetatable({}, { __tostring = function() return "mine" end })
            return tostring(t), string.format("%s %s", t, u)
        end"#, &[PortType::Text, PortType::Text]);
        assert_eq!(r.fault, None);
        assert_eq!(out, vec![PortValue::Text("table".into()), PortValue::Text("table mine".into())]);
    }

    #[test]
    fn inputs_and_outputs_follow_their_port_types() {
        let ins = [PortType::Scalar, PortType::Vec3, PortType::Stance, PortType::Track, PortType::TrackList, PortType::Bool, PortType::Text, PortType::Orders, PortType::PartState];
        let outs = [PortType::Angle, PortType::Vec3, PortType::Stance, PortType::Track, PortType::TrackList, PortType::Bool, PortType::Text, PortType::Emcon, PortType::Vec3, PortType::Scalar, PortType::Track, PortType::Orders, PortType::PartState];
        let text = r#"return function(x, v, st, t, list, b, s, orders, slot)
            local rev = {}
            for i = #list, 1, -1 do rev[#rev + 1] = list[i] end
            return x * 2, v + vector.create(1, 0, 0), st == "hostile" and "friend" or "hostile", t + 1, rev,
                not b, s .. "!", 2, { x = 1, y = 2, z = 3 }, "wrong type", 1.5, orders, slot
        end"#;
        let (mut h, r) = host_with(&[src(text, &ins, &outs)], lim());
        assert_eq!(r, vec![Ok(())]);
        let inputs = [
            PortValue::Scalar(2.5), PortValue::Vec3(DVec3::new(10.0, 0.0, -20.0)), PortValue::Stance(Stance::Hostile), PortValue::Track(TrackId(41)),
            PortValue::TrackList(Arc::new(vec![TrackId(1), TrackId(2), TrackId(3)])), PortValue::Bool(false), PortValue::Text("hi".into()), PortValue::Orders, PortValue::Part(PartSlot(4)),
        ];
        let (run, out) = run1(&mut h, 0, &inputs, outs.len());
        assert_eq!(run.fault, None);
        assert!(run.interrupts > 0);
        assert_eq!(out[0], PortValue::Scalar(5.0));
        assert_eq!(out[1], PortValue::Vec3(DVec3::new(11.0, 0.0, -20.0)));
        assert_eq!(out[2], PortValue::Stance(Stance::Friend));
        assert_eq!(out[3], PortValue::Track(TrackId(42)));
        assert_eq!(out[4], PortValue::TrackList(Arc::new(vec![TrackId(3), TrackId(2), TrackId(1)])));
        assert_eq!(out[5], PortValue::Bool(true));
        assert_eq!(out[6], PortValue::Text("hi!".into()));
        assert_eq!(out[7], PortValue::Emcon(Emcon::Active), "an enum output also takes its index");
        assert_eq!(out[8], PortValue::Vec3(DVec3::new(1.0, 2.0, 3.0)), "a table is a Vec3 too");
        assert_eq!(out[9], PortValue::None, "a value of the wrong type is no value");
        assert_eq!(out[10], PortValue::None, "a fractional track number is no track");
        assert_eq!(out[11], PortValue::Orders);
        assert_eq!(out[12], PortValue::Part(PartSlot(4)));
        // Missing inputs arrive as nil; missing outputs are no value.
        let (mut h, _) = host_with(&[src("return function(a) return a end", &[PortType::Scalar], &[PortType::Scalar, PortType::Scalar])], lim());
        let (run, out) = run1(&mut h, 0, &[PortValue::None], 2);
        assert_eq!((run.fault, out), (None, vec![PortValue::None, PortValue::None]));
    }

    #[test]
    fn detection_lists_pass_through_filtered_and_without_truth() {
        let det = |slot: u16, bearing: f64, range: Option<f64>| Detection {
            tick: 9, sensor: crate::ids::SensorRef { carrier: V, index: slot }, side: SideId(0), kind: content::part::SensorKind::SearchRadar, sensor_pos: DVec3::ZERO,
            target: EntityId { index: 99, generation: 4 },
            measurement: range.map_or(Measurement::BearingOnly { bearing_rad: bearing }, |r| Measurement::RangeBearing { range_m: r, bearing_rad: bearing, height_m: None }),
            sigma_range_m: 25.0, sigma_bearing_rad: 0.01, evidence: crate::sensors::Evidence::empty(), emitter: u16::MAX, rcs_m2: 50.0, domain_hint: Some(Domain::Air),
        };
        let list = vec![det(4, 0.5, Some(9_000.0)), det(5, 1.5, None), det(4, 2.5, Some(3_000.0))];
        let text = r#"return function(dets)
            local near = {}
            for _, d in ipairs(dets) do
                if d.target ~= nil or d.side ~= nil then error("truth leaked") end
                if d.range_m ~= nil and d.range_m < 5000 then near[#near + 1] = d end
            end
            near[#near + 1] = { bearing = 1 } -- not a detection the host gave: dropped
            return near, dets[2].bearing, dets[2].range_m == nil and dets[1].kind == "search_radar" and dets[1].domain == 2 and dets[1].sensor == 4
        end"#;
        let (mut h, _) = host_with(&[src(text, &[PortType::DetectionList], &[PortType::DetectionList, PortType::Angle, PortType::Bool])], lim());
        let (run, out) = run1(&mut h, 0, &[PortValue::Detections(Arc::new(list.clone()))], 3);
        assert_eq!(run.fault, None, "{:?}", h.last_error(V, 0));
        assert_eq!(out[0], PortValue::Detections(Arc::new(vec![list[2]])));
        assert_eq!(out[1], PortValue::Scalar(1.5));
        assert_eq!(out[2], PortValue::Bool(true));
    }

    #[test]
    fn chunk_state_persists_across_ticks_and_tick_globals_work() {
        let (mut h, r) = host_with(&[
            src("local n = 0 return function() n += 1 return n end", &[], &[PortType::Scalar]),
            src("count = 10 function tick() count = count + 1 return count end", &[], &[PortType::Scalar]),
        ], lim());
        assert_eq!(r, vec![Ok(()), Ok(())]);
        for want in 1..=3 {
            assert_eq!(run1(&mut h, 0, &[], 1).1[0], PortValue::Scalar(want as f64));
            assert_eq!(run1(&mut h, 1, &[], 1).1[0], PortValue::Scalar(10.0 + want as f64));
        }
    }

    #[test]
    fn scripts_on_one_vessel_and_on_two_vessels_never_share_globals() {
        let mut h = LuauHost::new();
        let w = src("return function() shared_name = 42 return 1 end", &[], &[PortType::Scalar]);
        let r = src("return function() return shared_name end", &[], &[PortType::Scalar]);
        assert_eq!(h.load(V, &[w.clone(), r.clone()], lim()), vec![Ok(()), Ok(())]);
        let other = EntityId { index: 4, generation: 1 };
        assert_eq!(h.load(other, &[r], lim()), vec![Ok(())]);
        let mut out = [PortValue::None];
        h.begin_tick(V);
        assert_eq!(h.run(V, 0, &[], &mut FixtureApi::default(), &mut out).fault, None);
        out[0] = PortValue::Scalar(-1.0);
        assert_eq!(h.run(V, 1, &[], &mut FixtureApi::default(), &mut out).fault, None);
        assert_eq!(out[0], PortValue::None, "the second script of the vessel does not see the first one's global");
        h.begin_tick(other);
        out[0] = PortValue::Scalar(-1.0);
        assert_eq!(h.run(other, 0, &[], &mut FixtureApi::default(), &mut out).fault, None);
        assert_eq!(out[0], PortValue::None);
        assert_eq!(h.vessels().collect::<Vec<_>>(), vec![V, other]);
        h.unload(V);
        assert_eq!(h.memory_bytes(V), 0);
        assert!(h.memory_bytes(other) > 0);
    }

    /// Fuzz crash e7e6a320 (`host_api`, base64 `//////8HrKysAGlm/wo=`): one call of `track_velocity` with
    /// a 151-level table constructor, inside `pcall`, 10,000 times. It used to spend its 5,000 units in
    /// 27 ms natively, 13 times the 2 ms backstop: each bad host call raised a Rust error the loop caught,
    /// and each iteration built 151 tables between two interrupts. Now the first bad call ends the run.
    #[test]
    fn work_the_interrupts_do_not_see_is_charged() {
        let ids = host_api().len() as u16 + 2;
        let crash = [FuzzCall { func: 65535 % ids, args: vec![FuzzArg::Deep(172)], protected: true, repeat: 65382 }];
        assert_eq!(crash[0].func, h::TRACK_VELOCITY);
        let recs = fuzz::check_calls(&crash).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].fault, Some(ScriptFault::Error), "{recs:?}");
        assert!(recs[0].interrupts < 100, "stopped after the first bad call, not at the budget: {recs:?}");
        // A host-call fault caught by `pcall` stops the run at the next interrupt; the host saw no call.
        let mut api = FixtureApi::default();
        let (mut hh, _) = host_with(&[src("return function() for _ = 1, 1e6 do pcall(track_velocity, {}) track_count() end return 1 end", &[], &[PortType::Scalar])], lim());
        hh.begin_tick(V);
        let r = hh.run(V, 0, &[], &mut api, &mut [PortValue::None]);
        assert_eq!((r.fault, api.calls), (Some(ScriptFault::Error), 0));
        assert!(r.interrupts < 20, "{r:?}");
        // Every caught error pays `CAUGHT_ERROR_UNITS`, through pcall, xpcall and coroutine.resume; the same
        // loop that catches nothing pays only its interrupts. Counts carry a memory-growth term that depends
        // on the allocator and the collector's phase, so they are bounded, not exact.
        let units = |text: &str| { let (r, out) = eval(text, &[PortType::Scalar]); assert_eq!((r.fault, &out[..]), (None, &[PortValue::Scalar(1.0)][..]), "{text}"); r.interrupts };
        let plain = units("return function() for _ = 1, 10 do pcall(function() end) end return 1 end");
        assert!(plain < 100, "{plain}");
        for text in [
            "return function() for _ = 1, 10 do pcall(error, 'x') end return 1 end",
            "return function() for _ = 1, 10 do xpcall(error, function(e) return e end) end return 1 end",
            "return function() for _ = 1, 10 do coroutine.resume(coroutine.create(error)) end return 1 end",
            "return function() for _ = 1, 10 do pcall(string.rep) end return 1 end",
        ] {
            let n = units(text);
            assert!(n >= 10 * CAUGHT_ERROR_UNITS && n <= plain + 10 * CAUGHT_ERROR_UNITS + 400, "{text}: {n}");
        }
        // A direct call of a charged builtin goes through its wrapper, not Luau's fast path (`NO_FASTCALL`):
        // the wrapper's own entry is one more interrupt per call.
        let direct = units("return function() local s = 'abc' for _ = 1, 200 do local t = string.sub(s, 1, 1) local b = string.byte(s, 1) end return 1 end");
        let bare = units("return function() local s = 'abc' for _ = 1, 200 do local t = s local b = 1 end return 1 end");
        assert!(direct >= bare + 400, "{direct} against {bare}");
        // A charged builtin's own argument error is a native Luau error that names the builtin.
        let (r, out) = eval("return function() local ok, e = pcall(string.rep) return e end", &[PortType::Text]);
        assert_eq!(r.fault, None);
        assert!(matches!(&out[0], PortValue::Text(t) if t.contains("'rep'")), "{out:?}");
        // A yield inside `pcall` still works through the guard.
        let (r, out) = eval("return function() local co = coroutine.create(function() local ok, v = pcall(function() return coroutine.yield(1) + 1 end) return v end)
            local _, a = coroutine.resume(co) local _, b = coroutine.resume(co, 41) return a + b end", &[PortType::Scalar]);
        assert_eq!((r.fault, &out[..]), (None, &[PortValue::Scalar(43.0)][..]));
        // Allocation between two interrupts is paid by the memory growth: a loop that builds 151 tables per
        // iteration gets 90 iterations out of the budget (about 2,500 before), the same on every run.
        let deep = format!("{}{}", "{".repeat(151), "}".repeat(151));
        let text = format!("return function() for _ = 1, 1e6 do local t = {deep} track_count() end end");
        let counts: Vec<_> = (0..2).map(|_| {
            let mut api = FixtureApi::default();
            let (mut hh, _) = host_with(&[src(&text, &[], &[])], lim());
            hh.begin_tick(V);
            let r = hh.run(V, 0, &[], &mut api, &mut []);
            (r.fault, api.calls)
        }).collect();
        assert_eq!(counts[0], counts[1]);
        assert_eq!(counts[0].0, Some(ScriptFault::Budget));
        assert!((20..=400).contains(&counts[0].1), "{counts:?}");
    }

    #[test]
    fn the_budget_stops_loops_even_through_pcall() {
        for text in [
            "return function() while true do end end",
            "return function() while true do pcall(function() while true do end end) end end",
            // Catches the budget error and returns normally: still a budget fault.
            "return function() pcall(function() while true do end end) return 1 end",
            "return function() local function f() return f() + 1 end return f() end",
            "return function() local s = string.rep('a', 64) for i = 1, 1e9 do s:find('a*a*a*a*a*b') end end",
            "return function() local t = {} for i = 1, 1e9 do t[i % 100] = i end end",
        ] {
            let (r, out) = eval(text, &[PortType::Scalar]);
            if text.contains("f() + 1") {
                // Unbounded recursion overflows the Luau stack before the budget: an error, never a crash.
                assert!(matches!(r.fault, Some(ScriptFault::Error | ScriptFault::Budget)), "{text}: {r:?}");
                continue;
            }
            assert_eq!(r.fault, Some(ScriptFault::Budget), "{text}");
            assert!(r.interrupts <= ScriptLimits::default().interrupts_per_tick + 1, "{text}: {}", r.interrupts);
            assert!(r.interrupts >= ScriptLimits::default().interrupts_per_tick, "{text}: {}", r.interrupts);
            assert_eq!(out, vec![PortValue::None], "outputs untouched on a fault");
        }
    }

    #[test]
    fn the_budget_is_shared_by_a_vessels_scripts_and_resets_each_tick() {
        let spend = "return function() for i = 1, 3000 do end return 1 end";
        let (mut h, _) = host_with(&[src(spend, &[], &[PortType::Scalar]), src(spend, &[], &[PortType::Scalar])], lim());
        h.begin_tick(V);
        h.begin_tick(V); // idempotent: the Sim and the graph VM both call it
        let mut out = [PortValue::None];
        let a = h.run(V, 0, &[], &mut FixtureApi::default(), &mut out);
        assert_eq!(a.fault, None);
        assert!((3_000..3_100).contains(&a.interrupts), "{}", a.interrupts);
        let b = h.run(V, 1, &[], &mut FixtureApi::default(), &mut out);
        assert_eq!(b.fault, Some(ScriptFault::Budget), "the second script finds the shared counter nearly spent");
        assert!(a.interrupts + b.interrupts <= 5_001);
        h.begin_tick(V);
        assert_eq!(h.run(V, 1, &[], &mut FixtureApi::default(), &mut out).fault, None, "a new tick, a new budget");
        // A smaller configured budget is honoured.
        let (mut h, _) = host_with(&[src(spend, &[], &[PortType::Scalar])], ScriptLimits { interrupts_per_tick: 500, ..lim() });
        assert_eq!(run1(&mut h, 0, &[], 1).0.fault, Some(ScriptFault::Budget));
        // The same script always uses the same count.
        let (mut h, _) = host_with(&[src(spend, &[], &[PortType::Scalar])], lim());
        let counts: Vec<u32> = (0..3).map(|_| run1(&mut h, 0, &[], 1).0.interrupts).collect();
        assert!(counts.windows(2).all(|w| w[0] == w[1]), "{counts:?}");
    }

    #[test]
    fn the_memory_limit_greys_out_allocating_scripts() {
        let limit = 8 * 1024 * 1024 + fuzz::MEMORY_SLACK;
        // Doubling a string by concatenation: few interrupts, fast growth. The growth is charged to the
        // budget (one unit per 128 bytes), so one tick stops it near 640 KB, long before the limit.
        let (mut h, _) = host_with(&[src("return function() local s = 'x' for i = 1, 30 do s = s .. s end return #s end", &[], &[PortType::Scalar])], lim());
        let (r, _) = run1(&mut h, 0, &[], 1);
        assert_eq!(r.fault, Some(ScriptFault::Budget), "{:?}", h.last_error(V, 0));
        assert!(h.memory_bytes(V) <= 2 * 1024 * 1024, "{}", h.memory_bytes(V));
        // Growth across ticks: state persists, so the limit is reached on some later tick. A linked list,
        // not one growing array: doubling an array past about 40,000 entries is one allocation larger than a
        // tick's growth allowance, and faults on the budget instead.
        let (mut h, _) = host_with(&[src("local head return function() for i = 1, 300 do head = {head, i, i} end return 1 end", &[], &[PortType::Scalar])], lim());
        let faulted = (0..2_000).find_map(|tick| { let (r, _) = run1(&mut h, 0, &[], 1); r.fault.map(|f| (tick, f)) });
        assert!(matches!(faulted, Some((t, ScriptFault::Memory)) if t > 10), "{faulted:?}");
        assert!(h.memory_bytes(V) <= limit, "{}", h.memory_bytes(V));
        // Sized constructors charge their size to the budget before they run: a size the budget cannot pay
        // for is a budget fault, decided before any memory is touched.
        for text in [
            "return function() return #string.rep('x', 16 * 1024 * 1024) end",
            "return function() return #string.rep('x', 7 * 1024 * 1024) end",
            "return function() local t = table.create(4000000, true) return #t end",
            "return function() return buffer.len(buffer.create(32 * 1024 * 1024)) end",
            "return function() local s = string.rep('x', 400000) for i = 1, 100 do s:upper() end return 1 end",
            // A string grown by `..` and then copied by direct calls, which Luau would fast-path.
            "return function() local s = 'x' for i = 1, 21 do s = s .. s end for i = 1, 100 do local t = string.sub(s, 1, #s - 1) end return 1 end",
            "return function() local s = 'x' for i = 1, 21 do s = s .. s end for i = 1, 100 do local t = s:sub(2) end return 1 end",
        ] {
            let (r, _) = eval(text, &[PortType::Scalar]);
            assert_eq!(r.fault, Some(ScriptFault::Budget), "{text}");
        }
        // Ordinary sizes cost next to nothing: 1 unit per 128 bytes, for the builtins' output and for the
        // `..` result's memory growth.
        let (r, out) = eval("return function() return #(string.rep('-', 80) .. string.rep('ab', 1000)) == 2080 and 2 or 0 end", &[PortType::Scalar]);
        assert_eq!((r.fault, &out[0]), (None, &PortValue::Scalar(2.0)));
        assert!(r.interrupts < 80, "{}", r.interrupts);
    }

    #[test]
    fn bad_sources_fail_to_load_with_the_right_fault() {
        let limits = lim();
        let cases: [(&str, ScriptFault); 6] = [
            ("return function( end", ScriptFault::Error),
            ("return 5", ScriptFault::Error),
            ("while true do end", ScriptFault::Budget),
            ("error('boom')", ScriptFault::Error),
            ("local p = own_position() return function() return p end", ScriptFault::Error),
            ("local t = {} for i = 1, 1e6 do t[i] = i end return function() end", ScriptFault::Budget),
        ];
        for (text, want) in cases {
            let (h, r) = host_with(&[src(text, &[], &[])], limits);
            assert_eq!(r, vec![Err(want)], "{text}");
            assert!(h.last_error(V, 0).is_some_and(|m| !m.is_empty()));
        }
        // Precompiled bytecode is never loaded, whatever it holds.
        let bytecode = Compiler::new().compile("return function() return 1 end").unwrap();
        let s = String::from_utf8_lossy(&bytecode).into_owned();
        let (_, r) = host_with(&[src(&s, &[], &[])], limits);
        assert_eq!(r, vec![Err(ScriptFault::Error)]);
        let (_, r) = host_with(&[src("\u{6}return function() end", &[], &[])], limits);
        assert_eq!(r, vec![Err(ScriptFault::Error)], "a version byte up front is refused, not run as bytecode");
        // Oversized sources.
        let big = format!("return function() end --{}", "x".repeat(MAX_SOURCE_BYTES));
        assert_eq!(host_with(&[src(&big, &[], &[])], limits).1, vec![Err(ScriptFault::Error)]);
        assert!(check_source(&big).is_err());
        // Disabled sources are skipped, never run; one bad source does not stop the others.
        let mut off = src("return function() return 1 end", &[], &[PortType::Scalar]);
        off.enabled = false;
        let good = src("return function() return 2 end", &[], &[PortType::Scalar]);
        let (mut h, r) = host_with(&[off.clone(), src("return", &[], &[]), good], limits);
        assert_eq!(r, vec![Ok(()), Err(ScriptFault::Error), Ok(())]);
        assert_eq!(run1(&mut h, 0, &[], 1).0.fault, Some(ScriptFault::Error));
        assert_eq!(run1(&mut h, 2, &[], 1).1[0], PortValue::Scalar(2.0));
        assert_eq!(run1(&mut h, 9, &[], 1).0.fault, Some(ScriptFault::Error), "no such script");
        let (h, r) = host_with(&[off], limits);
        assert_eq!((r, h.memory_bytes(V)), (vec![Ok(())], 0), "no VM for a design whose scripts are all off");
    }

    #[test]
    fn host_calls_follow_the_registry_and_charge_their_work() {
        let (r, out) = eval(r#"return function()
            local p = own_position()
            local n = track_count()
            local id = track_id(2)
            return p, n, id, track_index(id), track_stance(0), emcon(), bearing_to(vector.create(100, 0, -1200)), track_id(99), part_health(-1)
        end"#, &[PortType::Vec3, PortType::Scalar, PortType::Track, PortType::Scalar, PortType::Stance, PortType::Emcon, PortType::Angle, PortType::Track, PortType::Scalar]);
        assert_eq!(r.fault, None);
        assert_eq!(out[0], PortValue::Vec3(DVec3::new(100.0, 0.0, -200.0)));
        assert_eq!(out[1], PortValue::Scalar(5.0));
        assert_eq!(out[2], PortValue::Track(TrackId(5)));
        assert_eq!(out[3], PortValue::Scalar(2.0));
        assert_eq!(out[4], PortValue::Stance(Stance::Hostile));
        assert_eq!(out[5], PortValue::Emcon(Emcon::Burst));
        assert!(matches!(out[6], PortValue::Scalar(b) if b.abs() < 1e-9), "due north: {:?}", out[6]);
        assert_eq!((&out[7], &out[8]), (&PortValue::None, &PortValue::None), "missing things are nil, not faults");
        // Wrong arity or kind greys the script, even when caught.
        for text in ["return function() return own_position(1) end", "return function() return part_health() end",
            "return function() return part_health('a') end", "return function() return track_index({}) end",
            "return function() pcall(own_position, 1, 2) return 1 end", "return function() return no_such_fn() end"] {
            assert_eq!(eval(text, &[PortType::Scalar]).0.fault, Some(ScriptFault::Error), "{text}");
        }
        // Each call charges its declared work units: 1,000 track_index calls at 17 units overrun 5,000.
        let (r, _) = eval("return function() for i = 1, 1000 do track_index(5) end return 1 end", &[PortType::Scalar]);
        assert_eq!(r.fault, Some(ScriptFault::Budget));
        let (r, _) = eval("return function() for i = 1, 100 do own_speed_kn() end return 1 end", &[PortType::Scalar]);
        assert_eq!(r.fault, None);
        assert!(r.interrupts >= 200, "100 loop iterations and 100 calls: {}", r.interrupts);
    }

    #[test]
    fn a_host_function_kept_past_its_run_faults_cleanly() {
        // A coroutine created in one tick and resumed in a later one still finds an API: the slot is set per
        // run. Called from the chunk's top level, there is none and the load fails.
        let (mut h, r) = host_with(&[src(r#"
            local co = coroutine.wrap(function() while true do coroutine.yield(own_heading()) end end)
            return function() return co() end"#, &[], &[PortType::Angle])], lim());
        assert_eq!(r, vec![Ok(())]);
        for _ in 0..3 {
            let (run, out) = run1(&mut h, 0, &[], 1);
            assert_eq!((run.fault, out), (None, vec![PortValue::Scalar(0.54)]));
        }
        // Outside any run the slot is empty: calling a host function faults the load, never touches memory.
        let (h, r) = host_with(&[src("local x = track_count() return function() return x end", &[], &[PortType::Scalar])], lim());
        assert_eq!(r, vec![Err(ScriptFault::Error)]);
        assert!(h.last_error(V, 0).is_some_and(|m| m.contains("only be called while the script runs")), "{:?}", h.last_error(V, 0));
    }

    #[test]
    fn math_random_repeats_per_vessel() {
        let text = "return function() return math.random(), math.random(1, 1000000) end";
        let outs = [PortType::Scalar, PortType::Scalar];
        let a = eval(text, &outs).1;
        let b = eval(text, &outs).1;
        assert_eq!(a, b, "same vessel, same stream");
        let mut h = LuauHost::new();
        let other = EntityId { index: 11, generation: 2 };
        h.load(other, &[src(text, &[], &outs)], lim());
        let mut c = vec![PortValue::None; 2];
        h.begin_tick(other);
        h.run(other, 0, &[], &mut FixtureApi::default(), &mut c);
        assert_ne!(a, c, "another vessel draws another stream");
    }

    #[test]
    fn the_backstop_disables_a_script_that_runs_too_long() {
        let limits = ScriptLimits { backstop_micros: 1, interrupts_per_tick: u32::MAX / 2, ..ScriptLimits::default() };
        let (mut h, r) = host_with(&[src("return function() local s = 0 for i = 1, 1e8 do s += i end return s end", &[], &[PortType::Scalar])], limits);
        assert_eq!(r, vec![Ok(())]);
        let (run, out) = run1(&mut h, 0, &[], 1);
        assert_eq!(run.fault, Some(ScriptFault::Backstop), "{run:?} {:?}", h.last_error(V, 0));
        assert_eq!(out, vec![PortValue::None]);
        // At load, too.
        assert_eq!(host_with(&[src("for i = 1, 1e9 do end return function() end", &[], &[])], limits).1, vec![Err(ScriptFault::Backstop)]);
    }

    #[test]
    fn known_builtin_timeouts_are_closed() {
        // table.move over a huge sparse range used to loop 2^31 times without an interrupt: charged by its
        // count it faults at once, and the Luau fix keeps a sparse move fast for the sizes the budget allows.
        let (r, _) = eval("return function() local t = table.move({}, 1, 2^31 - 2, 2^30, {}) return #t end", &[PortType::Scalar]);
        assert_eq!(r.fault, Some(ScriptFault::Budget));
        let (r, out) = eval("return function() local t = table.move({}, 1, 30000, 2^30, {}) return #t end", &[PortType::Scalar]);
        assert_eq!((r.fault, &out[0]), (None, &PortValue::Scalar(0.0)));
        assert_eq!(Lua::set_fflag("LuauTableMoveTimeoutFix", true), Ok(()), "the pinned Luau still has the flag");
        // Pattern matching charges interrupts as it backtracks.
        let (r, _) = eval("return function() return string.find(string.rep('a', 40), string.rep('a*', 30) .. 'b') end", &[PortType::Scalar]);
        assert_eq!(r.fault, Some(ScriptFault::Budget));
        // Deep and cyclic data and huge counts never crash the host.
        for text in ["return function() local t = {} for i = 1, 200 do t = {t} end return t end",
            "return function() local t = {} t[1] = t return t end",
            "return function() return string.rep('x', -2^31) end",
            "return function() return table.unpack({}, 1, 2^31) end",
            "return function() return ('x'):rep(2^31) end",
            "return function() return string.format('%99999999d', 1) end"] {
            let (r, out) = eval(text, &[PortType::TrackList]);
            assert!(r.fault != Some(ScriptFault::Backstop), "{text}: {r:?}");
            assert!(matches!(&out[0], PortValue::None | PortValue::TrackList(_)));
        }
    }

    #[test]
    fn compile_check_reports_luau_errors() {
        assert_eq!(check_source("return function(a) return a * 2 end"), Ok(()));
        let e = check_source("return function(a) return a * end").unwrap_err();
        assert!(e.contains("Expected"), "{e}");
    }

    #[test]
    fn the_committed_fuzz_dictionary_matches_the_registry() {
        let committed = include_str!("../../../../fuzz/host_api.dict");
        assert_eq!(committed, crate::script::fuzz_dictionary(), "regenerate fuzz/host_api.dict from script::fuzz_dictionary()");
    }

    /// Shipped example scripts (L375: each under 1 MB).
    const EXAMPLES: [(&str, &str); 3] = [
        ("intercept", r#"-- Steer for the intercept point of the designated track at the speed given.
return function(speed_kn)
    local id = designated_track()
    if id == nil then return nil, nil end
    local i = track_index(id)
    if i == nil then return nil, nil end
    local p = intercept_point(track_position(i), track_velocity(i), (speed_kn or 20) * 0.5144)
    if p == nil then return nil, speed_kn end
    return bearing_to(p), speed_kn
end"#),
        ("nearest_hostile", r#"-- The nearest hostile track in the picture.
return function()
    local best, best_r = nil, math.huge
    for i = 0, track_count() - 1 do
        if track_stance(i) == "hostile" then
            local r = range_to(track_position(i))
            if r ~= nil and r < best_r then best, best_r = track_id(i), r end
        end
    end
    return best, best_r < math.huge and best_r or nil
end"#),
        ("zigzag", r#"-- Weave 30 degrees either side of the base course every 20 seconds.
local leg = 20
return function(base)
    if base == nil then return nil end
    local t = sortie_time_s()
    local side = (math.floor(t / leg) % 2 == 0) and 1 or -1
    return wrap_angle(base + side * math.rad(30)) % (2 * math.pi)
end"#),
    ];

    #[test]
    fn example_scripts_run_under_one_megabyte() {
        let ports: [(&[PortType], &[PortType]); 3] = [(&[PortType::Scalar], &[PortType::Angle, PortType::Scalar]), (&[], &[PortType::Track, PortType::Scalar]), (&[PortType::Angle], &[PortType::Angle])];
        for ((name, text), (ins, outs)) in EXAMPLES.iter().zip(ports) {
            let (mut h, r) = host_with(&[src(text, ins, outs)], lim());
            assert_eq!(r, vec![Ok(())], "{name}: {:?}", h.last_error(V, 0));
            let inputs: Vec<PortValue> = ins.iter().map(|_| PortValue::Scalar(1.0)).collect();
            let (run, out) = run1(&mut h, 0, &inputs, outs.len());
            assert_eq!(run.fault, None, "{name}: {:?}", h.last_error(V, 0));
            assert!(out.iter().any(|v| *v != PortValue::None), "{name}: {out:?}");
            assert!(h.memory_bytes(V) < 1024 * 1024, "{name}: {} bytes", h.memory_bytes(V));
            assert!(run.interrupts < 500, "{name}: {}", run.interrupts);
        }
        assert_eq!(fuzz::check_source_oracles(EXAMPLES[1].1).map(|s| s.len()), Ok(fuzz::TICKS as usize));
    }

    /// Seeds and regression inputs of the two fuzz targets: every one must pass every oracle.
    #[test]
    fn fuzz_regressions_pass_every_oracle() {
        let mut sources: Vec<String> = EXAMPLES.iter().map(|e| e.1.to_string()).collect();
        sources.extend([
            "", "return", "return function() end", "return function(...) return ... end", "\0\0\0", "\u{3}\u{1}", "--[[",
            "return function(a, b, c) return a, b, c, a, b, c, a, b end",
            "return function() return coroutine.wrap(function() coroutine.yield(1) end)() end",
            "return function() return coroutine.yield(1) end",
            "local t = setmetatable({}, { __index = function(t, k) return t[k] end }) return function() return t.x end",
            "return function() local t = setmetatable({}, { __tostring = function() return 'mine' end }) return 1, nil, tostring(t) end",
            "return function() return math.random(), math.noise(1.5, 2.5, 3.5) end",
            "return function() local t = {} for i = 1, 1000 do t[i] = tostring(i) end return #table.concat(t) end",
            "return function() return buffer.create(1024) end",
            "return function() return vector.create(0/0, math.huge, -math.huge) end",
            "return function() return string.format('%s %q', {}, 'x\\0y') end",
            "return function() error(setmetatable({}, { __tostring = function() while true do end end })) end",
            "return function() local ok, e = pcall(error) return ok, nil, tostring(e) end",
            "return function() local t = setmetatable({}, { __metatable = { __tostring = true } }) return 1, nil, tostring(t) .. string.format('%s', t) end",
            "return function() return utf8.char(0x10FFFF, 0x41), #utf8.char(72, 105) end",
            "return function() return xpcall(function() while true do end end, function(e) while true do end end) end",
        ].map(str::to_string));
        for s in &sources {
            fuzz::check_source_oracles(s).unwrap_or_else(|e| panic!("{e}\nsource: {s}"));
        }
        // Raw bytes, as the script_source target feeds them.
        fuzz::check_bytes(&[0xff, 0xfe, b'r', b'e', b't', b'u', b'r', b'n']).unwrap();
        // Generated host-call sequences with edge values.
        let all = [FuzzArg::Nil, FuzzArg::Bool(true), FuzzArg::Num(-0.0), FuzzArg::Nan, FuzzArg::Inf, FuzzArg::NegInf, FuzzArg::Huge, FuzzArg::NegCount, FuzzArg::Int(-7),
            FuzzArg::Vector(1.0, f32::NAN, 3.0), FuzzArg::Deep(150), FuzzArg::Cyclic, FuzzArg::Long(100_000), FuzzArg::Text("\"\\\u{7f}é".into()), FuzzArg::Function];
        let mut calls = Vec::new();
        for f in 0..(host_api().len() as u16 + 2) {
            let arity = host_api().get(f as usize).map_or(1, |x| x.args.len());
            for (k, a) in all.iter().enumerate() {
                calls.push(FuzzCall { func: f, args: vec![a.clone(); arity], protected: true, repeat: if k % 5 == 0 { 3 } else { 1 } });
            }
        }
        let recs = fuzz::check_calls(&calls).unwrap();
        assert!(matches!(recs[0].fault, Some(ScriptFault::Error | ScriptFault::Budget)), "wrong kinds grey the script even inside pcall: {:?}", recs[0]);
        // Crash e7e6a320 (see `work_the_interrupts_do_not_see_is_charged`), and the same shape through a
        // missing global, which raises a native error instead of a host fault.
        let ids = host_api().len() as u16 + 2;
        for func in [65535 % ids, host_api().len() as u16] {
            for n in [1, 8] {
                fuzz::check_calls(&[FuzzCall { func, args: vec![FuzzArg::Deep(172); n], protected: true, repeat: 65382 }]).unwrap();
            }
        }
        let first_bad: Vec<FuzzCall> = calls.iter().filter(|c| c.func == h::PART_HEALTH).cloned().collect();
        assert_eq!(fuzz::check_calls(&first_bad).unwrap()[0].fault, Some(ScriptFault::Error));
        // Well-typed edge values never fault: missing data is nil.
        let numeric = [FuzzArg::Nil, FuzzArg::Bool(false), FuzzArg::Num(-0.0), FuzzArg::Nan, FuzzArg::Inf, FuzzArg::NegInf, FuzzArg::Huge, FuzzArg::NegCount, FuzzArg::Int(-7), FuzzArg::Num(1e308)];
        let vectors = [FuzzArg::Nil, FuzzArg::Vector(f32::NAN, 0.0, 0.0), FuzzArg::Vector(f32::INFINITY, 0.0, -1.0), FuzzArg::Vector(1e30, -1e30, 0.0), FuzzArg::Vector(0.0, 0.0, 0.0)];
        let mut typed = Vec::new();
        for (f, def) in host_api().iter().enumerate() {
            for k in 0..numeric.len().max(vectors.len()) {
                let args = def.args.iter().map(|t| match t { PortType::Vec3 => vectors[k % vectors.len()].clone(), PortType::Track => FuzzArg::Int(k as i32 * 3 - 4), _ => numeric[k % numeric.len()].clone() }).collect();
                typed.push(FuzzCall { func: f as u16, args, protected: false, repeat: 1 });
            }
        }
        let recs = fuzz::check_calls(&typed).unwrap();
        assert_eq!(recs.len(), fuzz::TICKS as usize);
        assert!(recs.iter().all(|r| r.fault.is_none()), "{recs:?}");
        // Unprotected bad calls fault; long loops of calls hit the budget, never the backstop.
        let r = fuzz::check_calls(&[FuzzCall { func: h::PART_HEALTH, args: vec![FuzzArg::Cyclic], protected: false, repeat: 1 }]).unwrap();
        assert_eq!(r[0].fault, Some(ScriptFault::Error));
        let r = fuzz::check_calls(&[FuzzCall { func: h::INTERCEPT_POINT, args: vec![FuzzArg::Vector(1.0, 0.0, 5.0), FuzzArg::Vector(0.0, 0.0, 1.0), FuzzArg::Num(20.0)], protected: false, repeat: 10_000 }]).unwrap();
        assert_eq!(r[0].fault, Some(ScriptFault::Budget));
    }

    /// End to end: a starter hull whose helm sinks are driven by a script in a real Sim (Luau host, sim
    /// pool, officers, waves). The script steers due east at 14 kn, then throws at 45 s: the node greys out,
    /// the side hears `ScriptGreyedOut`, the helm officer takes the sinks back from that tick, and the ship
    /// stops spending interrupts.
    #[test]
    fn a_script_steers_a_real_ship_and_the_officer_takes_over_when_it_faults() {
        use crate::protocol::config::BlueprintRef;
        use crate::protocol::event::SimEvent;
        use content::graph::{GraphEdge, GraphNode, NodeKind, PortRef, SinkKind, USER_NODE_BASE};
        let catalogue = Arc::new(Catalogue::load_embedded().expect("catalogue"));
        let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign"));
        let data = crate::theatre::TheatreData { campaign: campaign.clone(), catalogue: catalogue.clone() };
        let g = crate::theatre::opgen::generate_standalone(content::campaign::MissionType::HunterGroupSurvival, Act::Act1, 1, 1, &Default::default(), None, &data).expect("an Act 1 sortie");
        let mut config = g.config;
        let own = config.players[0].vessel.expect("a player hull") as usize;
        let BlueprintRef::Stock(path) = config.vessels[own].blueprint.clone() else { panic!("a stock starter") };
        let mut bp = content::campaign::load::stock_blueprint(&path).expect("the starter");
        let node = GraphNodeId(USER_NODE_BASE + 77);
        bp.graph.scripts.push(ScriptSource {
            name: "east".into(), enabled: true, inputs: vec![],
            outputs: vec![("heading".into(), PortType::Angle), ("speed_kn".into(), PortType::Scalar)],
            source: "return function() if sortie_time_s() > 45 then error('planned fault') end return math.pi / 2, 14 end".into(),
        });
        bp.graph.pinned.nodes.push(GraphNode { id: node, kind: NodeKind::Script(0), params: Default::default(), pos: [0.0; 2] });
        for (port, sink) in [(0, SinkKind::HelmHeading), (1, SinkKind::HelmSpeed)] {
            bp.graph.pinned.edges.push(GraphEdge { from: PortRef { node, port }, to: PortRef { node: crate::graph::generate::sink_node(sink), port: 0 } });
        }
        let bp = crate::editor::splash::canonical(&bp);
        config.vessels[own].blueprint = BlueprintRef::Inline(bp.header.hash);
        // Start heading north, so the script has to turn the ship.
        config.vessels[own].heading_rad = 0.0;
        config.blueprints.push(bp);
        let deps = || crate::sim::SimDeps { catalogue: catalogue.clone(), campaign: campaign.clone(), designs: Default::default(), scripts: Box::new(LuauHost::new()), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
        let mut sim = crate::Sim::new(config, deps()).expect("the sortie starts");
        let vessel = sim.ships().of_player(PlayerSlot(0)).expect("the player's ship").vessel;
        assert!(sim.ships().get(vessel).unwrap().graph.scripts.len() == 1, "the script node compiled in");
        let off_east = |sim: &crate::Sim| {
            let h = sim.world().vessel(vessel).unwrap().kin.pose.heading_rad();
            let d = (h - core::f64::consts::FRAC_PI_2).rem_euclid(core::f64::consts::TAU);
            d.min(core::f64::consts::TAU - d)
        };
        let mut interrupts_before = 0u64;
        let mut greyed_at = None;
        assert!(off_east(&sim) > 1.5, "the ship starts heading north");
        for tick in 1..=(60 * 60) {
            sim.set_host_load(crate::lod::HostLoad::IDLE);
            sim.step();
            let work = sim.work().luau_interrupts as u64;
            for e in sim.drain_events() {
                if let SimEvent::ScriptGreyedOut { vessel: v, node: n, fault } = e.event {
                    assert_eq!((v, n, fault), (vessel, node, ScriptFault::Error));
                    greyed_at.get_or_insert(tick);
                }
            }
            if tick <= 44 * 60 { interrupts_before += work; }
            let helm = sim.ships().get(vessel).unwrap().sinks.get(SinkKind::HelmHeading).map(|w| w.origin);
            if tick == 20 * 60 {
                // A checkpoint restored into a fresh host loads the script again and it keeps the helm.
                let mut restored = crate::Sim::from_checkpoint(&sim.checkpoint(), deps()).expect("the checkpoint restores");
                for _ in 0..4 { restored.set_host_load(crate::lod::HostLoad::IDLE); restored.step(); }
                assert_eq!(restored.ships().get(vessel).and_then(|s| s.sinks.get(SinkKind::HelmHeading)).map(|w| w.origin), Some(crate::graph::WriteOrigin::Script(node)));
            }
            if tick == 44 * 60 {
                assert!(off_east(&sim) < 0.12, "the script steers due east: {:.3} rad off", off_east(&sim));
                assert!(interrupts_before > 0, "the script ran and was counted");
                assert_eq!(helm, Some(crate::graph::WriteOrigin::Script(node)), "the script holds the helm");
            }
            if greyed_at.is_some_and(|t| tick > t + 2) {
                assert!(matches!(helm, Some(crate::graph::WriteOrigin::Officer(_))), "the helm officer has the sink back: {helm:?}");
            }
            if greyed_at.is_some_and(|t| tick > t + 1) { assert_eq!(work, 0, "a greyed script costs nothing"); }
        }
        let at = greyed_at.expect("the planned fault greys the node");
        assert!((45 * 60..46 * 60).contains(&at), "greyed at tick {at}");
        let ship = sim.ships().get(vessel).unwrap();
        assert_eq!(ship.graph_state.greyed, vec![(node, ScriptFault::Error)], "stays greyed for the sortie");
    }

    /// Wall time per full-budget tick of the host_api fuzz surface and of the raw-source error and
    /// allocation paths, natively, with no backstop: anything far over the 2 ms production backstop is a
    /// path the work-unit budget does not pay for. Run with `cargo test -p sim-systems --features luau
    /// --lib -- --ignored --nocapture cost_sweep`. Nothing asserts on wall time.
    #[test]
    #[ignore]
    fn cost_sweep() {
        let none = ScriptLimits { backstop_micros: u32::MAX, ..ScriptLimits::default() };
        let time = |text: &str| -> (f64, Option<ScriptFault>, u32) {
            let (mut hh, r) = host_with(&[src(text, &[], &[PortType::Scalar])], none);
            if r != vec![Ok(())] { return (0.0, r[0].err(), 0); }
            let mut best = (f64::MAX, None, 0);
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let (run, _) = run1(&mut hh, 0, &[], 1);
                let ms = t.elapsed().as_secs_f64() * 1e3;
                if ms < best.0 { best = (ms, run.fault, run.interrupts); }
            }
            best
        };
        let mut rows: Vec<(f64, String)> = Vec::new();
        let args = [FuzzArg::Nil, FuzzArg::Num(1.0), FuzzArg::Nan, FuzzArg::Huge, FuzzArg::Int(2), FuzzArg::Vector(1.0, 2.0, 3.0), FuzzArg::Deep(150), FuzzArg::Cyclic,
            FuzzArg::Long(16 << 20), FuzzArg::Long(127), FuzzArg::Long(100_000), FuzzArg::Text("x".repeat(4096)), FuzzArg::Function];
        let funcs = [h::OWN_POSITION, h::TRACK_VELOCITY, h::INTERCEPT_POINT, host_api().len() as u16];
        for f in funcs {
            for a in &args {
                for n in [1usize, 8] {
                    for protected in [false, true] {
                        let calls = [FuzzCall { func: f, args: vec![a.clone(); n], protected, repeat: 10_000 }];
                        let (ms, fault, units) = time(&fuzz::render_host_calls(&calls));
                        rows.push((ms, format!("f{f} {n}x{a:?} protected {protected}: {fault:?} {units}").chars().take(140).collect()));
                    }
                }
            }
        }
        let deep = format!("{}{}", "{".repeat(151), "}".repeat(151));
        for text in [
            "return function() for _ = 1, 1e6 do pcall(error, 'x') end end".to_string(),
            "return function() for _ = 1, 1e6 do pcall(nil) end end".into(),
            "return function() for _ = 1, 1e6 do pcall(string.rep) end end".into(),
            "return function() for _ = 1, 1e6 do pcall(track_velocity, {}) end end".into(),
            "return function() for _ = 1, 1e6 do xpcall(error, error) end end".into(),
            "return function() for _ = 1, 1e6 do xpcall(error, function() error('y') end) end end".into(),
            "return function() for _ = 1, 1e6 do coroutine.resume(coroutine.create(error)) end end".into(),
            "return function() for _ = 1, 1e6 do pcall(coroutine.wrap(error)) end end".into(),
            "return function() for _ = 1, 1e6 do pcall(coroutine.wrap(function() coroutine.wrap(function() coroutine.wrap(error)() end)() end)) end end".into(),
            "return function() for _ = 1, 1e6 do coroutine.create(error) end end".into(),
            "local function chain(n) if n == 0 then error('x') end coroutine.wrap(chain)(n - 1) end return function() for _ = 1, 1e6 do pcall(chain, 190) end end".into(),
            "local function chain(n) if n == 0 then error('x') end local ok, e = pcall(chain, n - 1) error(e, 0) end return function() for _ = 1, 1e6 do pcall(chain, 190) end end".into(),
            "local function chain(n) if n == 0 then error('x') end coroutine.wrap(chain)(n - 1) end return function() for _ = 1, 1e6 do coroutine.resume(coroutine.create(chain), 190) end end".into(),
            "local function h(e) error(e) end return function() for _ = 1, 1e6 do xpcall(error, h) end end".into(),
            "return function() for _ = 1, 1e6 do local t = {} end end".into(),
            format!("return function() local last for _ = 1, 1e6 do last = {deep} end end"),
            "return function() local s = string.rep('x', 300000) for _ = 1, 1e6 do local t = s .. s end end".into(),
            "return function() while true do own_position() track_count() end end".into(),
        ] {
            let (ms, fault, units) = time(&text);
            rows.push((ms, format!("{fault:?} {units}: {}", text.chars().take(110).collect::<String>())));
        }
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (ms, what) in &rows[..rows.len().min(40)] { println!("{ms:9.3} ms  {what}"); }
    }

    /// The L375 budget gate's measurement, for the report: a script that spends the whole budget, timed per
    /// graph tick. Run with `cargo test --release -p sim-systems --features luau -- --ignored --nocapture
    /// budget_gate`. Nothing asserts on wall time in CI.
    #[test]
    #[ignore]
    fn budget_gate_benchmark() {
        let worst = [
            "return function() local s = 0 while true do s += 1 end end",
            "return function() local t = {} local i = 0 while true do i += 1 t[i % 64] = vector.create(i, i, i) * 2 end end",
            "return function() while true do own_position() track_count() end end",
            "return function() local s = string.rep('ab', 512) while true do s:find('b', 1, true) end end",
        ];
        for text in worst {
            let (mut h, _) = host_with(&[src(text, &[], &[PortType::Scalar])], lim());
            let mut ns: Vec<u64> = (0..2_000).map(|_| { let t = std::time::Instant::now(); let r = run1(&mut h, 0, &[], 1).0; assert_eq!(r.fault, Some(ScriptFault::Budget)); t.elapsed().as_nanos() as u64 }).collect();
            ns.sort_unstable();
            println!("p50 {:.3} ms, p99 {:.3} ms: {text}", ns[ns.len() / 2] as f64 / 1e6, ns[ns.len() * 99 / 100] as f64 / 1e6);
        }
    }
}
