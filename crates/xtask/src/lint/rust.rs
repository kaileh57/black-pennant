//! Unit `xt-lint-rust`. Workspace and Rust-source lints. Also owns `clippy.toml` at the repo root (L1111:
//! `disallowed-types` for `std::collections::HashMap` and `HashSet` in sim-core and sim-systems; clippy runs in
//! CI next to these greps).
//!
//! Scopes: sim crates = `crates/sim-core/src/`, `crates/sim-systems/src/`, `crates/content/src/` (determinism
//! lints); `crates/gdclient/src/` (mesh_clear); every `Cargo.toml` (features, deps); root `Cargo.toml`,
//! `rust-toolchain.toml`, `Cargo.lock` (pins). Comments and doc comments are ignored when grepping code.
use super::godot::{find_ident, finding, line_of};
use super::{Finding, LintCtx};

const SIM_DIRS: &[&str] = &["crates/sim-core/src/", "crates/sim-systems/src/", "crates/content/src/"];
const HASH_DIRS: &[&str] = &["crates/sim-core/src/", "crates/sim-systems/src/"];

// ------------------------------------------------------------------------------------------------
// Rust and TOML text helpers
// ------------------------------------------------------------------------------------------------

/// Rust source with comments removed (nested block comments included) and, if `blank_strings`, the contents
/// of string, byte-string, raw-string and char literals replaced by spaces. Byte length and newlines are kept,
/// so offsets and line numbers still match the original.
fn strip_rust(text: &str, blank_strings: bool) -> String {
    let b = text.as_bytes();
    let mut out: Vec<u8> = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for x in &mut out[from..to.min(b.len())] { if *x != b'\n' { *x = b' '; } }
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = b[i..].iter().position(|&x| x == b'\n').map_or(b.len(), |e| i + e);
            blank(&mut out, i, end);
            i = end;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1;
            let mut j = i + 2;
            while j < b.len() && depth > 0 {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') { depth += 1; j += 2; } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') { depth -= 1; j += 2; } else { j += 1; }
            }
            blank(&mut out, i, j);
            i = j;
            continue;
        }
        // Raw strings: r"..", r#".."#, br#".."#.
        let ident_before = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if !ident_before && (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r'))) {
            let start = if c == b'b' { i + 2 } else { i + 1 };
            let hashes = b[start..].iter().take_while(|&&x| x == b'#').count();
            if b.get(start + hashes) == Some(&b'"') {
                let body = start + hashes + 1;
                let mut j = body;
                let mut end = b.len();
                while j < b.len() {
                    if b[j] == b'"' && b[j + 1..].iter().take(hashes).filter(|&&x| x == b'#').count() == hashes && b.len() >= j + 1 + hashes {
                        end = j;
                        break;
                    }
                    j += 1;
                }
                if blank_strings { blank(&mut out, body, end); }
                i = (end + 1 + hashes).min(b.len());
                continue;
            }
        }
        if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' { if b[j] == b'\\' { j += 1; } j += 1; }
            if blank_strings { blank(&mut out, i + 1, j); }
            i = (j + 1).min(b.len());
            continue;
        }
        if c == b'\'' {
            // Char literal ('x', '\n', '\u{..}', multi-byte) or a lifetime / label.
            if b.get(i + 1) == Some(&b'\\') {
                let from = (i + 3).min(b.len());
                let end = b[from..].iter().position(|&x| x == b'\'').map_or(b.len(), |e| from + e);
                if blank_strings { blank(&mut out, i + 1, end); }
                i = (end + 1).min(b.len());
                continue;
            }
            let ch_len = text[i + 1..].chars().next().map_or(0, char::len_utf8);
            if ch_len > 0 && b.get(i + 1 + ch_len) == Some(&b'\'') {
                if blank_strings { blank(&mut out, i + 1, i + 1 + ch_len); }
                i += 2 + ch_len;
                continue;
            }
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

/// TOML text without `#` comments (strings kept, line structure kept).
fn strip_toml_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut q: Option<char> = None;
        let mut esc = false;
        let mut cut = line.len();
        for (i, c) in line.char_indices() {
            if let Some(qc) = q {
                if esc { esc = false; } else if c == '\\' && qc == '"' { esc = true; } else if c == qc { q = None; }
                continue;
            }
            match c { '"' | '\'' => q = Some(c), '#' => { cut = i; break; } _ => {} }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

fn toml_complete(v: &str) -> bool {
    let mut depth = 0i32;
    let mut q: Option<char> = None;
    for c in v.chars() {
        if let Some(qc) = q { if c == qc { q = None; } continue; }
        match c { '"' | '\'' => q = Some(c), '[' | '{' => depth += 1, ']' | '}' => depth -= 1, _ => {} }
    }
    q.is_none() && depth <= 0
}

/// One `[table]` (or `[[array]]` entry) of a TOML file.
#[derive(Clone, Debug)]
struct Table { name: String, line: u32, entries: Vec<(String, String, u32)> }

fn parse_toml(text: &str) -> Vec<Table> {
    let code = strip_toml_comments(text);
    let lines: Vec<&str> = code.lines().collect();
    let mut out = vec![Table { name: String::new(), line: 0, entries: Vec::new() }];
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        let ln = (i + 1) as u32;
        i += 1;
        if t.is_empty() { continue; }
        if t.starts_with('[') {
            let name = t.trim_start_matches('[').trim_end_matches(']').trim().to_string();
            out.push(Table { name, line: ln, entries: Vec::new() });
            continue;
        }
        let Some(eq) = t.find('=') else { continue };
        let key = t[..eq].trim().trim_matches('"').to_string();
        let mut value = t[eq + 1..].trim().to_string();
        while !toml_complete(&value) && i < lines.len() {
            value.push(' ');
            value.push_str(lines[i].trim());
            i += 1;
        }
        if let Some(tb) = out.last_mut() { tb.entries.push((key, value, ln)); }
    }
    out
}

fn toml_str(v: &str) -> Option<String> {
    let t = v.trim();
    (t.len() >= 2 && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))).then(|| t[1..t.len() - 1].to_string())
}

/// Split a TOML inline table or array body at top-level commas.
fn split_top(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut q: Option<char> = None;
    let mut cur = String::new();
    for c in v.chars() {
        if let Some(qc) = q { cur.push(c); if c == qc { q = None; } continue; }
        match c {
            '"' | '\'' => { q = Some(c); cur.push(c); }
            '[' | '{' => { depth += 1; cur.push(c); }
            ']' | '}' => { depth -= 1; cur.push(c); }
            ',' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    out
}

fn inline_table(v: &str) -> Vec<(String, String)> {
    let t = v.trim();
    let Some(body) = t.strip_prefix('{').and_then(|x| x.strip_suffix('}')) else { return Vec::new() };
    split_top(body).into_iter().filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().trim_matches('"').to_string(), v.trim().to_string()))).collect()
}

fn str_array(v: &str) -> Vec<String> {
    let t = v.trim();
    let Some(body) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) else { return Vec::new() };
    split_top(body).iter().filter_map(|s| toml_str(s)).collect()
}

/// A dependency declaration: name, the table it sits in, the fields of its spec, and its line.
#[derive(Clone, Debug)]
struct Dep { name: String, table: String, fields: Vec<(String, String)>, line: u32 }
impl Dep {
    fn field(&self, k: &str) -> Option<&str> { self.fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.as_str()) }
    fn version(&self) -> Option<String> { self.field("version").and_then(toml_str) }
    fn features(&self) -> Vec<String> { self.field("features").map(str_array).unwrap_or_default() }
    fn is_true(&self, k: &str) -> bool { self.field(k).map(str::trim) == Some("true") }
    fn package(&self) -> String { self.field("package").and_then(toml_str).unwrap_or_else(|| self.name.clone()) }
}

fn is_dep_table(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    matches!(last, "dependencies" | "dev-dependencies" | "build-dependencies" | "dev_dependencies" | "build_dependencies")
}

/// Every dependency in a manifest, from `[*dependencies]` tables (`name = "1"`, `name = { .. }`,
/// `name.workspace = true`) and `[*dependencies.name]` subtables.
fn deps(tables: &[Table]) -> Vec<Dep> {
    let mut out: Vec<Dep> = Vec::new();
    for t in tables {
        if is_dep_table(&t.name) {
            for (k, v, ln) in &t.entries {
                let (name, fields) = match k.split_once('.') {
                    Some((n, f)) => (n.trim().trim_matches('"').to_string(), vec![(f.trim().to_string(), v.clone())]),
                    None if v.trim().starts_with('{') => (k.clone(), inline_table(v)),
                    None => (k.clone(), vec![("version".to_string(), v.clone())]),
                };
                match out.iter_mut().find(|d| d.name == name && d.table == t.name) {
                    Some(d) => d.fields.extend(fields),
                    None => out.push(Dep { name, table: t.name.clone(), fields, line: *ln }),
                }
            }
        } else if let Some((table, name)) = t.name.rsplit_once('.') {
            if is_dep_table(table) {
                out.push(Dep { name: name.trim_matches('"').to_string(), table: table.to_string(), fields: t.entries.iter().map(|(k, v, _)| (k.clone(), v.clone())).collect(), line: t.line });
            }
        }
    }
    out
}

fn manifests(ctx: &LintCtx) -> Vec<&str> {
    ctx.files("", &["toml"]).filter(|f| *f == "Cargo.toml" || f.ends_with("/Cargo.toml")).collect()
}

// ------------------------------------------------------------------------------------------------
// std_transcendentals
// ------------------------------------------------------------------------------------------------

const TRANSCENDENTALS: &[&str] = &[
    "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh", "exp", "exp2", "exp_m1",
    "ln", "ln_1p", "log", "log10", "log2", "powf", "powi", "hypot", "cbrt", "sin_cos",
];

/// Method names a sim crate defines itself; `.name(` calls on those are ambiguous and left to clippy's typed
/// `disallowed-methods` check, while `f64::name(` stays flagged.
fn defined_fns(ctx: &LintCtx) -> Vec<String> {
    let mut out = Vec::new();
    for dir in SIM_DIRS {
        for f in ctx.files(dir, &["rs"]) {
            let code = strip_rust(&ctx.read(f), true);
            for name in TRANSCENDENTALS {
                for at in find_ident(&code, "fn") {
                    if code[at + 2..].trim_start().strip_prefix(*name).map_or(false, |r| r.trim_start().starts_with(['(', '<'])) && !out.iter().any(|o| o == name) {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out
}

fn check_transcendentals(path: &str, text: &str, user_defined: &[String]) -> Vec<Finding> {
    const L: &str = "rust.transcendentals";
    let code = strip_rust(text, true);
    let mut out = Vec::new();
    for name in TRANSCENDENTALS {
        for at in find_ident(&code, name) {
            let after = code[at + name.len()..].trim_start();
            if !(after.starts_with('(') || after.starts_with("::<")) { continue; }
            let before = code[..at].trim_end();
            let method = before.ends_with('.') && !before.ends_with("..");
            let path_call = ["f64::", "f32::", "<f64>::", "<f32>::", "core::f64::", "std::f64::"].iter().any(|p| before.ends_with(p)) || before.ends_with("f64 ::") || before.ends_with("f32 ::");
            if path_call || (method && !user_defined.iter().any(|u| u == name)) {
                out.push(finding(L, path, line_of(&code, at), format!("std {name}: transcendental results differ across platforms; call libm::{name} (L441)")));
            }
        }
    }
    out
}

/// Calls to std float transcendentals (`.sin(`, `.cos(`, `.tan(`, `.exp(`, `.ln(`, `.powf(`, `.powi(`, `.atan2(`,
/// `.asin(`, `.acos(`, `.atan(`, `.sinh(` ..., `.log(`, `.log10(`, `.log2(`, `.exp2(`, `.hypot(`, `.cbrt(`,
/// `.sin_cos(`) in sim crates. Allowed: `libm::` calls, `sqrt`, `abs`, `floor`, `ceil`, `round`, `trunc`,
/// `mul_add`. Unit: `xt-lint-rust`.
pub fn std_transcendentals(ctx: &LintCtx) -> Vec<Finding> {
    let user = defined_fns(ctx);
    let mut out = Vec::new();
    for dir in SIM_DIRS {
        for f in ctx.files(dir, &["rs"]) { out.extend(check_transcendentals(f, &ctx.read(f), &user)); }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// hash_collections
// ------------------------------------------------------------------------------------------------

fn check_hash(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "rust.hash-iteration";
    let code = strip_rust(text, true);
    let mut out = Vec::new();
    for ident in ["HashMap", "HashSet", "hash_map", "hash_set"] {
        for at in find_ident(&code, ident) {
            out.push(finding(L, path, line_of(&code, at), format!("std {ident}: iteration order is random per process; use BTreeMap, IndexMap with a fixed hasher, or ascending-id vectors (L1111)")));
        }
    }
    out
}

/// `HashMap` or `HashSet` from std (or `std::collections::hash_map`) used in sim-core or sim-systems.
/// Unit: `xt-lint-rust`.
pub fn hash_collections(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for dir in HASH_DIRS {
        for f in ctx.files(dir, &["rs"]) { out.extend(check_hash(f, &ctx.read(f))); }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// parallel_float_sum
// ------------------------------------------------------------------------------------------------

const PAR_STARTS: &[&str] = &["par_iter", "par_iter_mut", "into_par_iter", "par_chunks", "par_chunks_mut", "par_chunks_exact", "par_bridge", "par_windows", "par_drain", "par_split"];
const REDUCERS: &[&str] = &["sum", "product", "reduce", "reduce_with", "fold", "fold_with", "fold_chunks", "try_fold", "try_reduce", "try_reduce_with"];
const INT_TYPES: &[&str] = &["u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize"];

fn has_float_evidence(s: &str) -> bool {
    if !find_ident(s, "f64").is_empty() || !find_ident(s, "f32").is_empty() { return true; }
    let b = s.as_bytes();
    (1..b.len().saturating_sub(1)).any(|i| b[i] == b'.' && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit() && (i < 2 || !(b[i - 2] == b'.')))
        || s.contains("_f64") || s.contains("_f32")
}

fn has_int_evidence(s: &str) -> bool {
    INT_TYPES.iter().any(|t| s.contains(&format!("::<{t}>")) || s.contains(&format!(": {t} ")) || s.contains(&format!(": {t}=")) || s.contains(&format!("-> {t}")) || s.contains(&format!("_{t}")))
}

fn check_parallel(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "rust.parallel-float-sum";
    let code = strip_rust(text, true);
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut reported: Vec<usize> = Vec::new();
    for start in PAR_STARTS {
        for at in find_ident(&code, start) {
            // Statement extent: back to the previous `;`, `{` or `}`, forward to the `;` that closes it.
            let s0 = code[..at].rfind([';', '{', '}']).map_or(0, |p| p + 1);
            let mut depth = 0i32;
            let mut e = at;
            while e < b.len() {
                match b[e] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => { depth -= 1; if depth < 0 { break; } }
                    b';' if depth == 0 => break,
                    _ => {}
                }
                e += 1;
            }
            let stmt = &code[s0..e];
            let chain = &code[at..e];
            let reducer = REDUCERS.iter().find(|r| find_ident(chain, r).iter().any(|&p| chain[..p].trim_end().ends_with('.')));
            let Some(r) = reducer else { continue };
            if reported.contains(&s0) { continue; }
            if has_float_evidence(stmt) || !has_int_evidence(stmt) {
                reported.push(s0);
                let why = if has_float_evidence(stmt) { "over floats" } else { "of a type not shown to be an integer (write .sum::<u64>() or similar)" };
                out.push(finding(L, path, line_of(&code, at), format!("parallel {r} {why}; merge parallel work serially in a fixed order (L1152)")));
            }
        }
    }
    out
}

/// rayon parallel iterators ending in `sum`, `reduce`, `fold` or `product` over floats in sim crates.
/// Unit: `xt-lint-rust`.
pub fn parallel_float_sum(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for dir in SIM_DIRS {
        for f in ctx.files(dir, &["rs"]) { out.extend(check_parallel(f, &ctx.read(f))); }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// experimental_threads
// ------------------------------------------------------------------------------------------------

fn check_experimental(path: &str, text: &str) -> Vec<Finding> {
    let code = if path.ends_with(".toml") { strip_toml_comments(text) } else { text.to_string() };
    code.lines().enumerate().filter(|(_, l)| l.contains("experimental-threads") || l.contains("experimental_threads")).map(|(i, _)| {
        finding("rust.experimental-threads", path, (i + 1) as u32, "gdext experimental-threads is never enabled; bindings stay on the main thread (L1088)")
    }).collect()
}

/// `experimental-threads` anywhere in a Cargo.toml. Unit: `xt-lint-rust`.
pub fn experimental_threads(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in manifests(ctx) { out.extend(check_experimental(f, &ctx.read(f))); }
    // Feature flags can also arrive through cargo config aliases and CI command lines.
    for f in ctx.files(".cargo/", &["toml"]).chain(ctx.files(".github/", &["yml", "yaml"])) {
        let text = ctx.read(f);
        let code = if f.ends_with(".toml") { text } else { text.lines().map(|l| l.split(" #").next().unwrap_or(l)).collect::<Vec<_>>().join("\n") };
        out.extend(check_experimental(f, &code));
    }
    out
}

// ------------------------------------------------------------------------------------------------
// godot_scope
// ------------------------------------------------------------------------------------------------

const GDCLIENT_MANIFEST: &str = "crates/gdclient/Cargo.toml";

fn is_godot_crate(name: &str) -> bool { name == "godot" || name.starts_with("godot-") || name.starts_with("godot_") }

fn check_godot_manifest(path: &str, text: &str) -> Vec<Finding> {
    if path == GDCLIENT_MANIFEST { return Vec::new(); }
    deps(&parse_toml(text)).into_iter().filter(|d| !d.table.starts_with("workspace") && is_godot_crate(&d.package())).map(|d| {
        finding("rust.godot-scope", path, d.line, format!("{} dependency outside gdclient; only gdclient depends on Godot (L1060-1072)", d.package()))
    }).collect()
}

/// Workspace packages (no `source`) other than gdclient whose locked dependencies include a godot crate.
fn check_godot_lock(text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in parse_toml(text).iter().filter(|t| t.name == "package") {
        let get = |k: &str| t.entries.iter().find(|(e, _, _)| e == k).map(|(_, v, _)| v.clone());
        let name = get("name").and_then(|v| toml_str(&v)).unwrap_or_default();
        if get("source").is_some() || name == "gdclient" || is_godot_crate(&name) { continue; }
        for dep in get("dependencies").map(|v| str_array(&v)).unwrap_or_default() {
            let dn = dep.split_whitespace().next().unwrap_or("");
            if is_godot_crate(dn) {
                out.push(finding("rust.godot-scope", "Cargo.lock", t.line, format!("{name} depends on {dn}; only gdclient depends on Godot (L1060-1072)")));
            }
        }
    }
    out
}

/// A `godot` dependency in any crate but gdclient. Unit: `xt-lint-rust`.
pub fn godot_scope(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in manifests(ctx) { out.extend(check_godot_manifest(f, &ctx.read(f))); }
    if ctx.files.iter().any(|f| f == "Cargo.lock") { out.extend(check_godot_lock(&ctx.read("Cargo.lock"))); }
    out
}

// ------------------------------------------------------------------------------------------------
// steam_default_off
// ------------------------------------------------------------------------------------------------

const STEAM_DEPS: &[&str] = &["steamworks", "renet2_steam", "steamworks-sys"];

/// Features enabled by `default`, transitively within the crate.
fn default_closure(features: &[(String, String, u32)]) -> Vec<String> {
    let get = |k: &str| features.iter().find(|(f, _, _)| f == k).map(|(_, v, _)| str_array(v)).unwrap_or_default();
    let mut on: Vec<String> = get("default");
    let mut i = 0;
    while i < on.len() {
        let f = on[i].clone();
        for sub in get(&f) { if !on.contains(&sub) { on.push(sub); } }
        i += 1;
    }
    on
}

fn check_steam_manifest(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "rust.steam-default-off";
    let tables = parse_toml(text);
    let mut out = Vec::new();
    if let Some(ft) = tables.iter().find(|t| t.name == "features") {
        let line = ft.entries.iter().find(|(k, _, _)| k == "default").map_or(ft.line, |(_, _, l)| *l);
        if let Some(f) = default_closure(&ft.entries).into_iter().find(|f| f.to_ascii_lowercase().contains("steam")) {
            out.push(finding(L, path, line, format!("default features enable {f}; the steam feature is off by default (L1290)")));
        }
    }
    for d in deps(&tables) {
        if d.table.starts_with("workspace") || !STEAM_DEPS.contains(&d.package().as_str()) { continue; }
        if !d.is_true("optional") {
            out.push(finding(L, path, d.line, format!("{} must be optional, enabled only by the steam feature (L1290)", d.package())));
        }
    }
    out
}

fn steam_sdk_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let lower = path.to_ascii_lowercase();
    matches!(name.as_str(), "steam_api.dll" | "steam_api64.dll" | "libsteam_api.so" | "libsteam_api.dylib" | "steam_api.lib" | "steam_api64.lib" | "steam_api.h" | "steam_api_flat.h" | "steam_api.json")
        || name.starts_with("sdkencryptedappticket")
        || lower.contains("steamworks_sdk")
        || lower.contains("sdk/redistributable_bin/")
        || lower.contains("sdk/public/steam/")
}

/// `steam` in any crate's `default` features; tracked Steamworks SDK files or `steam_api` binaries.
/// Unit: `xt-lint-rust`.
pub fn steam_default_off(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in manifests(ctx) { out.extend(check_steam_manifest(f, &ctx.read(f))); }
    for f in ctx.files.iter().filter(|f| steam_sdk_file(f)) {
        out.push(finding("rust.steam-default-off", f, 0, "Steamworks SDK file in the repository; the SDK is never committed (L1290)"));
    }
    out
}

// ------------------------------------------------------------------------------------------------
// version_pins
// ------------------------------------------------------------------------------------------------

/// (crate, exact pin, required features, banned features, must be present).
const PINS: &[(&str, &str, &[&str], &[&str], bool)] = &[
    ("godot", "=0.5.5", &["api-4-7"], &[], true),
    ("rapier3d-f64", "=0.36.1", &["parallel", "enhanced-determinism"], &["simd8", "simd-stable", "simd-nightly"], true),
    ("libm", "=0.2.16", &[], &[], true),
    ("binary-greedy-meshing", "=0.6.2", &[], &[], true),
    ("fundsp", "=0.23.0", &[], &[], true),
    ("renet2", "=1.0.0", &[], &[], true),
    ("renet2_netcode", "=1.0.0", &[], &[], true),
    ("mlua", "=0.12.2", &["luau"], &["luau-jit"], false),
    ("renet2_steam", "=1.0.0", &[], &[], false),
    ("steamworks", "=0.13.1", &[], &[], false),
];

fn check_root_pins(text: &str) -> Vec<Finding> {
    const L: &str = "rust.version-pins";
    let tables = parse_toml(text);
    let ws: Vec<Dep> = deps(&tables).into_iter().filter(|d| d.table == "workspace.dependencies").collect();
    let mut out = Vec::new();
    for (name, pin, need, banned, required) in PINS {
        let Some(d) = ws.iter().find(|d| d.name == *name) else {
            if *required { out.push(finding(L, "Cargo.toml", 0, format!("[workspace.dependencies] has no {name} pin ({pin}; L1180-1224)"))); }
            continue;
        };
        if d.version().as_deref() != Some(*pin) {
            out.push(finding(L, "Cargo.toml", d.line, format!("{name} is {}, must be {pin} (L1180-1224)", d.version().unwrap_or_else(|| "unpinned".into()))));
        }
        let feats = d.features();
        for f in *need {
            if !feats.iter().any(|x| x == f) { out.push(finding(L, "Cargo.toml", d.line, format!("{name} needs feature {f} (L1180-1224)"))); }
        }
        for f in *banned {
            if feats.iter().any(|x| x == f) { out.push(finding(L, "Cargo.toml", d.line, format!("{name} must never enable {f} (L1096, L1180-1224)"))); }
        }
        if d.field("default-features").map(str::trim) == Some("false") && (*name == "godot" || *name == "libm") {
            out.push(finding(L, "Cargo.toml", d.line, format!("{name} keeps its default features (L441, L1180-1224)")));
        }
    }
    out
}

/// Member manifests use the workspace pins and never add banned features.
fn check_member_pins(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "rust.version-pins";
    let mut out = Vec::new();
    for d in deps(&parse_toml(text)) {
        if d.table.starts_with("workspace") { continue; }
        let pkg = d.package();
        let Some((name, pin, _, banned, _)) = PINS.iter().find(|p| p.0 == pkg) else { continue };
        // Either inherit the workspace pin, or (for optional crates the workspace does not list, such as the
        // feature-gated Steam backend) pin the exact version in the member itself.
        if !d.is_true("workspace") && d.version().as_deref() != Some(*pin) {
            out.push(finding(L, path, d.line, format!("{name} must use the pin: {name}.workspace = true, or version = \"{pin}\"")));
        }
        for f in d.features() {
            if banned.contains(&f.as_str()) { out.push(finding(L, path, d.line, format!("{name} must never enable {f} (L1096)"))); }
        }
    }
    out
}

fn check_toolchain(toolchain: Option<&str>, root: &str) -> Vec<Finding> {
    const L: &str = "rust.version-pins";
    let Some(tc) = toolchain else { return vec![finding(L, "rust-toolchain.toml", 0, "rust-toolchain.toml is missing (L1190)")] };
    let mut out = Vec::new();
    let channel = parse_toml(tc).iter().find(|t| t.name == "toolchain").and_then(|t| t.entries.iter().find(|(k, _, _)| k == "channel").map(|(_, v, l)| (toml_str(v).unwrap_or_default(), *l)));
    let Some((channel, ln)) = channel else { return vec![finding(L, "rust-toolchain.toml", 0, "no [toolchain] channel")] };
    let parts: Vec<&str> = channel.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        out.push(finding(L, "rust-toolchain.toml", ln, format!("channel {channel} is not an exact x.y.z release (L1190)")));
    }
    if channel == "1.99.0" { out.push(finding(L, "rust-toolchain.toml", ln, "1.99.0 miscompiles extern \"C\" bool returns on x86_64 (L1206)")); }
    let rv = parse_toml(root).iter().find(|t| t.name == "workspace.package").and_then(|t| t.entries.iter().find(|(k, _, _)| k == "rust-version").and_then(|(_, v, _)| toml_str(v)));
    if rv.as_deref() != Some(channel.as_str()) {
        out.push(finding(L, "Cargo.toml", 0, format!("workspace rust-version {} does not match rust-toolchain.toml {channel} (L1196)", rv.unwrap_or_else(|| "unset".into()))));
    }
    out
}

/// Locked versions of the pinned crates match their pins.
fn check_lock_pins(lock: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for t in parse_toml(lock).iter().filter(|t| t.name == "package") {
        let get = |k: &str| t.entries.iter().find(|(e, _, _)| e == k).and_then(|(_, v, _)| toml_str(v));
        let (Some(name), Some(ver)) = (get("name"), get("version")) else { continue };
        if let Some((_, pin, ..)) = PINS.iter().find(|p| p.0 == name) {
            if pin.trim_start_matches('=') != ver {
                out.push(finding("rust.version-pins", "Cargo.lock", t.line, format!("{name} {ver} is locked; the pin is {pin} (L1180)")));
            }
        }
    }
    out
}

/// Root pins: `godot = "=0.5.5"` with `api-4-7`; `rapier3d-f64 = "=0.36.1"` with `parallel` and
/// `enhanced-determinism`, never `simd8`; `libm = "=0.2.16"`; `binary-greedy-meshing = "=0.6.2"`;
/// `fundsp = "=0.23.0"`; `renet2 = "=1.0.0"`; `mlua = "=0.12.2"` when present; `rust-toolchain.toml` and
/// `Cargo.lock` exist. Unit: `xt-lint-rust`.
pub fn version_pins(ctx: &LintCtx) -> Vec<Finding> {
    let has = |p: &str| ctx.files.iter().any(|f| f == p);
    let root = ctx.read("Cargo.toml");
    let mut out = check_root_pins(&root);
    for f in manifests(ctx).into_iter().filter(|f| *f != "Cargo.toml") { out.extend(check_member_pins(f, &ctx.read(f))); }
    let tc = has("rust-toolchain.toml").then(|| ctx.read("rust-toolchain.toml"));
    out.extend(check_toolchain(tc.as_deref(), &root));
    if has("Cargo.lock") {
        out.extend(check_lock_pins(&ctx.read("Cargo.lock")));
    } else {
        out.push(finding("rust.version-pins", "Cargo.lock", 0, "Cargo.lock is not committed (L1180)"));
    }
    out
}

// ------------------------------------------------------------------------------------------------
// mesh_clear
// ------------------------------------------------------------------------------------------------

fn check_mesh_clear(path: &str, text: &str) -> Vec<Finding> {
    let code = strip_rust(text, false);
    find_ident(&code, "mesh_clear").into_iter().map(|at| {
        finding("rust.mesh-clear", path, line_of(&code, at), "mesh_clear frees a visible mesh at once; build a new RID and swap it with instance_set_base (L1929)")
    }).collect()
}

/// `mesh_clear` in gdclient sources. Unit: `xt-lint-rust`.
pub fn mesh_clear(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("crates/gdclient/src/", &["rs"]) { out.extend(check_mesh_clear(f, &ctx.read(f))); }
    out
}

// ------------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripper_keeps_offsets() {
        let src = "let a = \"x.sin()\"; // y.cos()\n/* outer /* inner */ z.tan() */ let b = r#\"q.exp()\"#; let c = '\\''; let d: &'static str = \"s\";\n";
        let s = strip_rust(src, true);
        assert_eq!(s.len(), src.len());
        assert_eq!(s.lines().count(), src.lines().count());
        for w in ["sin", "cos", "tan", "exp"] { assert!(!s.contains(w), "{w} left in {s}"); }
        assert!(s.contains("'static"));
        let kept = strip_rust("let p = \"res://_placeholder/x\"; // c\n", false);
        assert!(kept.contains("_placeholder"));
    }

    #[test]
    fn transcendentals() {
        let src = "fn f(x: f64) -> f64 {\n    let a = x.sin();\n    let b = libm::sin(x);\n    let c = f64::cos(x);\n    let d = x.sqrt().abs();\n    let e = x.powi(2);\n    // x.exp()\n    a + b + c + d + e\n}\n";
        let f = check_transcendentals("crates/sim-core/src/a.rs", src, &[]);
        assert_eq!(f.iter().map(|f| f.line).collect::<Vec<_>>(), vec![2, 4, 6], "{f:#?}");
        let f = check_transcendentals("crates/sim-core/src/a.rs", "fn g(t: &Tracer) { t.log(\"x\"); }\n", &["log".to_string()]);
        assert!(f.is_empty());
    }

    #[test]
    fn hash_collections_in_code_not_comments() {
        let src = "//! never iterate std HashMap\nuse std::collections::{BTreeMap, HashSet};\nlet m: IndexMap<u32, u32> = IndexMap::new();\nlet n = \"HashMap\";\n";
        assert_eq!(check_hash("crates/sim-core/src/a.rs", src).len(), 1);
    }

    #[test]
    fn parallel_reductions() {
        let bad = "fn f(v: &[f64]) -> f64 { v.par_iter().map(|x| x * 2.0).sum() }\n";
        assert_eq!(check_parallel("a.rs", bad).len(), 1);
        let unknown = "fn f(v: &[Thing]) { let t = v.par_iter().map(|x| x.weight).sum(); }\n";
        assert_eq!(check_parallel("a.rs", unknown).len(), 1);
        let int = "fn f(v: &[u32]) -> u64 { v.par_iter().map(|&x| x as u64).sum::<u64>() }\n";
        assert!(check_parallel("a.rs", int).is_empty());
        let collect = "fn f(v: &[f64]) -> Vec<f64> { v.par_iter().map(|x| x * 2.0).collect() }\n";
        assert!(check_parallel("a.rs", collect).is_empty());
        let fold = "let total: f32 = items.into_par_iter().fold(|| 0.0, |a, b| a + b).reduce(|| 0.0, |a, b| a + b);\n";
        assert_eq!(check_parallel("a.rs", fold).len(), 1);
    }

    #[test]
    fn experimental_threads_ignores_comments() {
        assert_eq!(check_experimental("crates/gdclient/Cargo.toml", "godot = { workspace = true, features = [\"experimental-threads\"] }\n").len(), 1);
        assert!(check_experimental("crates/gdclient/Cargo.toml", "# never experimental-threads\ngodot.workspace = true\n").is_empty());
    }

    #[test]
    fn godot_scope_manifests_and_lock() {
        let m = "[package]\nname = \"net\"\n\n[dependencies]\ngodot.workspace = true\nserde = \"1\"\n\n[target.'cfg(windows)'.dependencies]\ngd = { package = \"godot\", version = \"0.5\" }\n";
        assert_eq!(check_godot_manifest("crates/net/Cargo.toml", m).len(), 2);
        assert!(check_godot_manifest(GDCLIENT_MANIFEST, m).is_empty());
        assert!(check_godot_manifest("Cargo.toml", "[workspace.dependencies]\ngodot = { version = \"=0.5.5\" }\n").is_empty());
        let lock = "[[package]]\nname = \"gdclient\"\nversion = \"0.1.0\"\ndependencies = [\n \"godot\",\n]\n\n[[package]]\nname = \"server\"\nversion = \"0.1.0\"\ndependencies = [\n \"godot 0.5.5\",\n \"serde\",\n]\n\n[[package]]\nname = \"godot\"\nversion = \"0.5.5\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\ndependencies = [\n \"godot-core\",\n]\n";
        let f = check_godot_lock(lock);
        assert_eq!(f.len(), 1);
        assert!(f[0].message.starts_with("server"));
    }

    #[test]
    fn steam_rules() {
        let ok = "[features]\ndefault = []\nsteam = [\"dep:renet2_steam\", \"dep:steamworks\"]\n\n[dependencies]\nrenet2_steam = { workspace = true, optional = true }\nsteamworks = { workspace = true, optional = true }\n";
        assert!(check_steam_manifest("crates/net/Cargo.toml", ok).is_empty());
        let bad = "[features]\ndefault = [\"online\"]\nonline = [\"steam\"]\nsteam = [\"dep:steamworks\"]\n\n[dependencies]\nsteamworks.workspace = true\n";
        assert_eq!(check_steam_manifest("crates/net/Cargo.toml", bad).len(), 2);
        assert!(steam_sdk_file("vendor/steam/redistributable_bin/win64/steam_api64.dll"));
        assert!(steam_sdk_file("third_party/steamworks_sdk_161/readme.txt"));
        assert!(!steam_sdk_file("crates/net/src/steam.rs"));
    }

    #[test]
    fn pins() {
        let root = "[workspace.package]\nrust-version = \"1.99.1\"\n\n[workspace.dependencies]\ngodot = { version = \"=0.5.5\", features = [\"api-4-7\"] }\nrapier3d-f64 = { version = \"=0.36.1\", features = [\"parallel\", \"enhanced-determinism\"] }\nrenet2 = \"=1.0.0\"\nrenet2_netcode = { version = \"=1.0.0\", features = [\"memory_transport\"] }\nbinary-greedy-meshing = \"=0.6.2\"\nfundsp = \"=0.23.0\"\n# pinned\nlibm = \"=0.2.16\"\n";
        assert!(check_root_pins(root).is_empty(), "{:#?}", check_root_pins(root));
        let bad = root.replace("\"=0.36.1\", features = [\"parallel\", \"enhanced-determinism\"]", "\"0.36\", features = [\"parallel\", \"simd8\"]").replace("libm = \"=0.2.16\"\n", "");
        // version, missing enhanced-determinism, simd8, missing libm.
        assert_eq!(check_root_pins(&bad).len(), 4, "{:#?}", check_root_pins(&bad));
        let member = "[dependencies]\nrapier3d-f64 = { workspace = true, features = [\"simd8\"] }\nlibm = \"0.2\"\nfundsp.workspace = true\n";
        assert_eq!(check_member_pins("crates/sim-core/Cargo.toml", member).len(), 2);
        let steam = "[dependencies]\nrenet2_steam = { version = \"=1.0.0\", optional = true }\nsteamworks = { version = \"0.13\", optional = true }\n";
        assert_eq!(check_member_pins("crates/net/Cargo.toml", steam).len(), 1, "exact member pin passes, loose one fails");
        let tc = "[toolchain]\nchannel = \"1.99.1\"\n";
        assert!(check_toolchain(Some(tc), root).is_empty());
        assert_eq!(check_toolchain(Some("[toolchain]\nchannel = \"1.99.0\"\n"), root).len(), 2);
        assert_eq!(check_toolchain(None, root).len(), 1);
        let lock = "[[package]]\nname = \"libm\"\nversion = \"0.2.15\"\nsource = \"registry+x\"\n";
        assert_eq!(check_lock_pins(lock).len(), 1);
    }

    #[test]
    fn mesh_clear_in_code() {
        assert_eq!(check_mesh_clear("crates/gdclient/src/a.rs", "// never mesh_clear\nrs.mesh_clear(rid);\n").len(), 1);
    }
}
