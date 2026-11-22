//! Unit `xt-lint-godot`. Godot project, scene, environment, particle and shader lints.
//! Scopes: `game/project.godot`; `game/**/*.tscn`, `*.tres` (environments, cameras, particles); `game/**/*.gdshader`
//! and `*.gdshaderinc` (water shaders under `game/shaders/water/`, VFX under `game/shaders/vfx/`); `game/**/*.gd`.
//! Gameplay scenes are every scene outside `game/ui/` and `game/tools/`. Parsing is line-based on Godot's text
//! formats (`[node name=... type="..."]`, `key = value`).
//!
//! This file also holds the Godot text-format helpers the UI and copy lints share (`pub(super)`): the
//! `.tscn`/`.tres` section parser, the node tree, value parsers, the `project.godot` reader and comment strippers
//! for GDScript and shaders. `game/test/` (lint fixtures and test scenes) and `game/addons/` (vendored code) are
//! outside every Godot lint.
use super::{Finding, LintCtx, Severity};

// ------------------------------------------------------------------------------------------------
// Shared helpers
// ------------------------------------------------------------------------------------------------

pub(super) fn finding(lint: &'static str, path: &str, line: u32, message: impl Into<String>) -> Finding {
    Finding { lint, severity: Severity::Error, path: path.to_string(), line, message: message.into() }
}

/// Directories no Godot-side lint reads: lint fixtures and tests, vendored add-ons.
pub(super) fn exempt(path: &str) -> bool { path.starts_with("game/test/") || path.starts_with("game/addons/") }

/// Scenes and resources that draw the 3D world: everything under `game/` outside the UI, tools, tests,
/// add-ons and the placeholder quarantine.
pub(super) fn is_world_path(path: &str) -> bool {
    path.starts_with("game/")
        && !exempt(path)
        && !path.starts_with("game/ui/")
        && !path.starts_with("game/tools/")
        && !path.starts_with("game/_placeholder/")
}

/// `res://a/b` to the repo-relative `game/a/b`.
pub(super) fn res_to_repo(res: &str) -> Option<String> { res.strip_prefix("res://").map(|r| format!("game/{r}")) }

/// One property line (`key = value`, value possibly spanning lines).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Prop { pub key: String, pub value: String, pub line: u32 }

/// One bracketed section of a Godot text scene or resource with the properties that follow it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Section { pub kind: String, pub attrs: Vec<(String, String)>, pub props: Vec<Prop>, pub line: u32 }

impl Section {
    pub fn attr(&self, k: &str) -> Option<&str> { self.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str()) }
    /// Attribute value with its quotes removed.
    pub fn attr_str(&self, k: &str) -> Option<String> { self.attr(k).map(unquote) }
    /// The last value of a property (Godot keeps the last one).
    pub fn prop(&self, k: &str) -> Option<&Prop> { self.props.iter().rev().find(|p| p.key == k) }
    pub fn type_name(&self) -> Option<String> { self.attr_str("type") }
}

/// Bracket and string balance of a value, to know whether it continues on the next line.
fn value_complete(v: &str) -> bool {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for c in v.chars() {
        if in_str {
            if esc { esc = false; } else if c == '\\' { esc = true; } else if c == '"' { in_str = false; }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    !in_str && depth <= 0
}

/// Parse the attributes of a section header body (`node name="X" type="Y" parent="."`).
fn parse_attrs(body: &str) -> (String, Vec<(String, String)>) {
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let mut kind = String::new();
    while i < chars.len() && !chars[i].is_whitespace() { kind.push(chars[i]); i += 1; }
    let mut attrs = Vec::new();
    loop {
        while i < chars.len() && chars[i].is_whitespace() { i += 1; }
        if i >= chars.len() { break; }
        let mut key = String::new();
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() { key.push(chars[i]); i += 1; }
        while i < chars.len() && chars[i].is_whitespace() { i += 1; }
        if i >= chars.len() || chars[i] != '=' {
            if !key.is_empty() { attrs.push((key, String::new())); }
            continue;
        }
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() { i += 1; }
        let mut val = String::new();
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        while i < chars.len() {
            let c = chars[i];
            if in_str {
                val.push(c);
                if esc { esc = false; } else if c == '\\' { esc = true; } else if c == '"' { in_str = false; }
                i += 1;
                continue;
            }
            if c.is_whitespace() && depth == 0 { break; }
            match c {
                '"' => in_str = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
            val.push(c);
            i += 1;
        }
        attrs.push((key, val));
    }
    (kind, attrs)
}

/// Parse a `.tscn` or `.tres` text into sections. Properties before the first header are dropped.
pub(super) fn parse_resource(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        let ln = (i + 1) as u32;
        i += 1;
        if t.is_empty() || t.starts_with(';') { continue; }
        if t.starts_with('[') && t.ends_with(']') {
            let (kind, attrs) = parse_attrs(&t[1..t.len() - 1]);
            out.push(Section { kind, attrs, props: Vec::new(), line: ln });
            continue;
        }
        let Some((eq, w)) = t.find(" = ").map(|e| (e, 3)).or_else(|| t.find('=').map(|e| (e, 1))) else { continue };
        let key = t[..eq].trim().to_string();
        let mut value = t[eq + w..].trim().to_string();
        while !value_complete(&value) && i < lines.len() {
            value.push('\n');
            value.push_str(lines[i].trim_end_matches('\r'));
            i += 1;
        }
        if let Some(s) = out.last_mut() { s.props.push(Prop { key, value, line: ln }); }
    }
    out
}

/// A node of a text scene.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Node {
    pub name: String,
    /// Empty for instanced scenes whose root type the file does not say.
    pub ty: String,
    /// `None` for the scene root.
    pub parent: Option<String>,
    /// Path from the root: `.` for the root, `A/B` below it (Godot's `get_node_path` without `./`).
    pub path: String,
    pub props: Vec<Prop>,
    pub line: u32,
    pub instance: Option<String>,
}
impl Node {
    pub fn prop(&self, k: &str) -> Option<&Prop> { self.props.iter().rev().find(|p| p.key == k) }
    pub fn prop_val(&self, k: &str) -> Option<&str> { self.prop(k).map(|p| p.value.as_str()) }
}

pub(super) fn scene_nodes(sections: &[Section]) -> Vec<Node> {
    let mut out = Vec::new();
    for s in sections.iter().filter(|s| s.kind == "node") {
        let name = s.attr_str("name").unwrap_or_default();
        let parent = s.attr_str("parent");
        let path = match parent.as_deref() {
            None => ".".to_string(),
            Some(".") | Some("") => name.clone(),
            Some(p) => format!("{}/{}", p.trim_start_matches("./"), name),
        };
        out.push(Node { name, ty: s.type_name().unwrap_or_default(), parent, path, props: s.props.clone(), line: s.line, instance: s.attr("instance").map(str::to_string) });
    }
    out
}

/// Path of a node's parent in [`Node::path`] form.
pub(super) fn parent_path(n: &Node) -> Option<String> {
    match n.parent.as_deref() {
        None => None,
        Some(".") | Some("") => Some(".".to_string()),
        Some(p) => Some(p.trim_start_matches("./").to_string()),
    }
}

/// Remove surrounding quotes and the `&` / `^` StringName and NodePath prefixes.
pub(super) fn unquote(v: &str) -> String {
    let v = v.trim();
    let v = v.strip_prefix('&').or_else(|| v.strip_prefix('^')).unwrap_or(v);
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut esc = false;
        for c in inner.chars() {
            if esc {
                out.push(match c { 'n' => '\n', 't' => '\t', other => other });
                esc = false;
            } else if c == '\\' { esc = true; } else { out.push(c); }
        }
        out
    } else {
        v.to_string()
    }
}

pub(super) fn parse_num(v: &str) -> Option<f64> {
    let t = v.trim();
    if t == "inf" { return Some(f64::INFINITY); }
    t.parse::<f64>().ok().filter(|n| !n.is_nan())
}
pub(super) fn parse_bool(v: &str) -> Option<bool> {
    match v.trim() { "true" => Some(true), "false" => Some(false), _ => None }
}

/// The numbers inside a constructor call like `Color(0.1, 0.2, 0.3, 1)`; `None` if any argument is not a
/// numeric literal.
pub(super) fn ctor_args(v: &str, ctor: &str) -> Option<Vec<f64>> {
    let t = v.trim();
    let rest = t.strip_prefix(ctor)?.trim_start();
    let inner = rest.strip_prefix('(')?;
    let close = inner.find(')')?;
    let args = &inner[..close];
    if args.trim().is_empty() { return Some(Vec::new()); }
    args.split(',').map(parse_num).collect()
}

/// Every `SubResource("id")` (or other `ctor("id")`) argument in a value.
pub(super) fn resource_refs(v: &str, ctor: &str) -> Vec<String> {
    let mut out = Vec::new();
    let pat = format!("{ctor}(");
    let mut rest = v;
    while let Some(i) = rest.find(&pat) {
        let after = &rest[i + pat.len()..];
        if let Some(end) = after.find(')') { out.push(unquote(&after[..end])); rest = &after[end..]; } else { break; }
    }
    out
}

/// `project.godot` / ConfigFile entries as (`section/key`, value, line), multi-line values joined.
pub(super) fn parse_config(text: &str) -> Vec<(String, String, u32)> {
    let mut out = Vec::new();
    let mut section = String::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        let ln = (i + 1) as u32;
        i += 1;
        if t.is_empty() || t.starts_with(';') || t.starts_with('#') { continue; }
        if t.starts_with('[') && t.ends_with(']') { section = t[1..t.len() - 1].trim().to_string(); continue; }
        let Some(eq) = t.find('=') else { continue };
        let key = t[..eq].trim();
        let mut value = t[eq + 1..].trim().to_string();
        while !value_complete(&value) && i < lines.len() {
            value.push('\n');
            value.push_str(lines[i].trim_end_matches('\r'));
            i += 1;
        }
        let full = if section.is_empty() { key.to_string() } else { format!("{section}/{key}") };
        out.push((full, value, ln));
    }
    out
}

/// GDScript without `#` comments (strings, including triple-quoted ones, kept). Newlines are preserved.
pub(super) fn strip_gd_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' { i += 1; }
            continue;
        }
        if c == '"' || c == '\'' {
            let triple = i + 2 < chars.len() && chars[i + 1] == c && chars[i + 2] == c;
            let n = if triple { 3 } else { 1 };
            for _ in 0..n { out.push(c); }
            i += n;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' && i + 1 < chars.len() { out.push(d); out.push(chars[i + 1]); i += 2; continue; }
                if !triple && d == '\n' { break; }
                if d == c && (!triple || (i + 2 < chars.len() && chars[i + 1] == c && chars[i + 2] == c)) {
                    for _ in 0..n { out.push(c); }
                    i += n;
                    break;
                }
                out.push(d);
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Shader source without `//` and `/* */` comments (strings kept, newlines preserved).
pub(super) fn strip_c_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            out.push(c);
            i += 1;
            while i < chars.len() && chars[i] != '"' && chars[i] != '\n' { out.push(chars[i]); i += 1; }
            if i < chars.len() && chars[i] == '"' { out.push('"'); i += 1; }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' { i += 1; }
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/') {
                if chars[i] == '\n' { out.push('\n'); }
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Byte offsets of `needle` in `hay` (ASCII case-insensitive) where the characters on both sides are not
/// letters or digits. Underscores count as boundaries, so `film_grain` contains `grain`.
pub(super) fn find_word_ci(hay: &str, needle: &str) -> Vec<usize> {
    let lower = hay.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    let mut out = Vec::new();
    if n.is_empty() { return out; }
    let n_first = n.chars().next().unwrap_or(' ').is_alphanumeric();
    let n_last = n.chars().next_back().unwrap_or(' ').is_alphanumeric();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&n) {
        let at = from + rel;
        let before = lower[..at].chars().next_back();
        let after = lower[at + n.len()..].chars().next();
        let ok_b = before.map_or(true, |c| !(c.is_alphanumeric() && n_first));
        let ok_a = after.map_or(true, |c| !(c.is_alphanumeric() && n_last));
        if ok_b && ok_a { out.push(at); }
        from = at + n.len();
    }
    out
}

/// Identifier occurrences of `ident` (case-sensitive; neighbours are not identifier characters).
pub(super) fn find_ident(hay: &str, ident: &str) -> Vec<usize> {
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    if ident.is_empty() { return out; }
    let mut from = 0;
    while let Some(rel) = hay[from..].find(ident) {
        let at = from + rel;
        let b = hay[..at].chars().next_back();
        let a = hay[at + ident.len()..].chars().next();
        if b.map_or(true, |c| !is_id(c)) && a.map_or(true, |c| !is_id(c)) { out.push(at); }
        from = at + ident.len();
    }
    out
}

pub(super) fn line_of(text: &str, byte: usize) -> u32 { (text.as_bytes()[..byte.min(text.len())].iter().filter(|&&b| b == b'\n').count() + 1) as u32 }

/// Value assigned to `prop` in GDScript: `x.prop = V`, `prop = V` or `set("prop", V)`. Yields (line, value text).
pub(super) fn gd_assignments(code: &str, prop: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (i, line) in code.lines().enumerate() {
        for at in find_ident(line, prop) {
            let rest = line[at + prop.len()..].trim_start();
            if let Some(v) = rest.strip_prefix('=') {
                if v.starts_with('=') { continue; }
                out.push(((i + 1) as u32, v.trim().trim_end_matches(';').trim().to_string()));
            } else if rest.starts_with('"') || rest.starts_with('\'') {
                // set("prop", V)
                let before = line[..at].trim_end();
                if before.ends_with("set(\"") || before.ends_with("set('") || before.ends_with("set(&\"") {
                    if let Some(v) = rest[1..].trim_start().strip_prefix(',') {
                        let v = v.trim();
                        let end = v.rfind(')').unwrap_or(v.len());
                        out.push(((i + 1) as u32, v[..end].trim().to_string()));
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// renderer_keys
// ------------------------------------------------------------------------------------------------

const PROJECT: &str = "game/project.godot";

/// (key, required value) pairs from L1268 and L1092.
const PINNED_KEYS: &[(&str, &str)] = &[
    ("rendering/renderer/rendering_method", "\"forward_plus\""),
    ("rendering/rendering_device/driver.windows", "\"d3d12\""),
    ("rendering/rendering_device/driver.macos", "\"metal\""),
    ("rendering/rendering_device/driver.linuxbsd", "\"vulkan\""),
    ("rendering/rendering_device/fallback_to_vulkan", "true"),
    ("rendering/rendering_device/fallback_to_d3d12", "true"),
    ("rendering/rendering_device/fallback_to_opengl3", "true"),
    ("physics/3d/physics_engine", "\"Dummy\""),
];

/// L1268 keys in `game/project.godot`: `rendering/renderer/rendering_method="forward_plus"`,
/// `rendering/rendering_device/driver.windows="d3d12"`, `driver.macos="metal"`, `driver.linuxbsd="vulkan"`,
/// `fallback_to_vulkan`, `fallback_to_d3d12`, `fallback_to_opengl3` true; L1092 `physics/3d/physics_engine`
/// set to the dummy server. Missing or changed fails. Unit: `xt-lint-godot`.
pub fn renderer_keys(ctx: &LintCtx) -> Vec<Finding> {
    if !ctx.files.iter().any(|f| f == PROJECT) {
        return vec![finding("godot.renderer-keys", PROJECT, 0, "game/project.godot is missing")];
    }
    check_renderer_keys(PROJECT, &ctx.read(PROJECT))
}

fn check_renderer_keys(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.renderer-keys";
    let entries = parse_config(text);
    let mut out = Vec::new();
    for (key, want) in PINNED_KEYS {
        match entries.iter().rev().find(|(k, _, _)| k == key) {
            None => out.push(finding(L, path, 0, format!("{key} is missing; pin it to {want} (L1268, L1092)"))),
            Some((_, v, ln)) if v.trim() != *want => out.push(finding(L, path, *ln, format!("{key} is {}, must be {want} (L1268, L1092)", v.trim()))),
            _ => {}
        }
        // Feature-tag overrides (`key.mobile`, `key.web`, ...) change the pinned value on some platform.
        for (k, v, ln) in &entries {
            if k.len() > key.len() && k.starts_with(key) && k.as_bytes()[key.len()] == b'.' && v.trim() != *want {
                out.push(finding(L, path, *ln, format!("{k} overrides the pinned {key} ({want}) with {}", v.trim())));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// physics_bodies
// ------------------------------------------------------------------------------------------------

/// VehicleBody3D is a RigidBody3D subclass.
const BANNED_BODIES: &[&str] = &["RigidBody3D", "CharacterBody3D", "VehicleBody3D"];

/// `type="RigidBody3D"` or `type="CharacterBody3D"` in gameplay scenes. Unit: `xt-lint-godot`.
pub fn physics_bodies(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["tscn", "tres", "gd"]) {
        if !is_world_path(f) { continue; }
        out.extend(check_physics_bodies(f, &ctx.read(f)));
    }
    out
}

fn check_physics_bodies(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.physics-bodies";
    let mut out = Vec::new();
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        for body in BANNED_BODIES {
            for at in find_ident(&code, body) {
                let rest = &code[at + body.len()..];
                let before = code[..at].trim_end();
                if rest.starts_with(".new") || before.ends_with("extends") {
                    out.push(finding(L, path, line_of(&code, at), format!("{body} in a gameplay script; gameplay physics is Rapier in Rust (L1092)")));
                }
            }
        }
        return out;
    }
    for s in parse_resource(text) {
        if s.kind != "node" { continue; }
        if let Some(t) = s.type_name() {
            if BANNED_BODIES.contains(&t.as_str()) {
                out.push(finding(L, path, s.line, format!("{t} node in a gameplay scene; gameplay physics is Rapier in Rust (L1092)")));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// environment
// ------------------------------------------------------------------------------------------------

/// Godot 4 default for `Environment.fog_density`.
const DEFAULT_FOG_DENSITY: f64 = 0.01;

fn env_const(v: &str) -> Option<f64> {
    let t = v.trim();
    let name = t.rsplit('.').next().unwrap_or(t);
    let n = match name {
        "GLOW_BLEND_MODE_ADDITIVE" => 0.0,
        "GLOW_BLEND_MODE_SCREEN" => 1.0,
        "GLOW_BLEND_MODE_SOFTLIGHT" => 2.0,
        "GLOW_BLEND_MODE_REPLACE" => 3.0,
        "GLOW_BLEND_MODE_MIX" => 4.0,
        "BG_CLEAR_COLOR" => 0.0,
        "BG_COLOR" => 1.0,
        "BG_SKY" => 2.0,
        "BG_CANVAS" => 3.0,
        "BG_KEEP" => 4.0,
        "BG_CAMERA_FEED" => 5.0,
        "true" => 1.0,
        "false" => 0.0,
        _ => return parse_num(t),
    };
    Some(n)
}

/// The property checks shared by resources and scripts. `get` returns a property's (line, value text).
fn check_env_values(path: &str, get: &dyn Fn(&str) -> Option<(u32, String)>, glow_on: Option<bool>, section_line: u32) -> Vec<Finding> {
    const L: &str = "godot.environment";
    let mut out = Vec::new();
    let num = |k: &str| get(k).and_then(|(ln, v)| env_const(&v).map(|n| (ln, n)));
    if let Some((ln, v)) = num("glow_hdr_threshold") {
        if v < 1.0 { out.push(finding(L, path, ln, format!("glow_hdr_threshold {v} is below 1.0 (L1847)"))); }
    }
    match num("glow_blend_mode") {
        Some((ln, v)) if v != 1.0 => out.push(finding(L, path, ln, format!("glow_blend_mode {v}; must be 1, Screen (L1847)"))),
        None if glow_on == Some(true) => out.push(finding(L, path, section_line, "glow is on but glow_blend_mode is not set; set it to 1, Screen (L1847)")),
        _ => {}
    }
    if let Some((ln, v)) = num("glow_bloom") {
        if v != 0.0 { out.push(finding(L, path, ln, format!("glow_bloom {v}; bloom stays 0 (L1847)"))); }
    }
    if let Some((ln, v)) = num("auto_exposure_enabled") {
        if v != 0.0 { out.push(finding(L, path, ln, "auto_exposure_enabled = true; auto exposure stays off (L1848)")); }
    }
    if let Some((ln, v)) = num("fog_height_density") {
        if v != 0.0 { out.push(finding(L, path, ln, format!("fog_height_density {v}; must be 0 (L2061)"))); }
    }
    if let Some((ln, a)) = num("fog_aerial_perspective") {
        let density = num("fog_density").map_or(DEFAULT_FOG_DENSITY, |(_, d)| d);
        if a > 0.3 && density >= 5e-4 {
            out.push(finding(L, path, ln, format!("fog_aerial_perspective {a} above 0.3 at fog density {density} (L2061)")));
        }
    }
    if let Some((ln, v)) = num("background_mode") {
        if v == 3.0 { out.push(finding(L, path, ln, "background_mode is Canvas; world environments use BG_SKY so glow and LUT never reach the HUD (L1851)")); }
    }
    for k in ["ssil_enabled", "sdfgi_enabled"] {
        if let Some((ln, v)) = num(k) {
            if v != 0.0 { out.push(finding(L, path, ln, format!("{k} = true; SSIL and SDFGI are always off (L1850)"))); }
        }
    }
    out
}

/// World `Environment` resources: `glow_hdr_threshold` < 1.0, `glow_blend_mode` != 1, glow bloom != 0,
/// `auto_exposure_enabled = true`, `fog_height_density` != 0, aerial perspective > 0.3 where density >= 5e-4,
/// `ProceduralSkyMaterial` in a world environment, `background_mode` canvas, SSIL/SDFGI on. Unit: `xt-lint-godot`.
pub fn environment(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["tscn", "tres", "gd"]) {
        if exempt(f) { continue; }
        out.extend(check_environment(f, &ctx.read(f)));
    }
    out
}

fn check_environment(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.environment";
    const SKY: &str = "ProceduralSkyMaterial in a world environment; the sky is the project's sky shader (L2061)";
    let mut out = Vec::new();
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        let get = |k: &str| gd_assignments(&code, k).into_iter().last();
        out.extend(check_env_values(path, &get, None, 0));
        for at in find_ident(&code, "ProceduralSkyMaterial") {
            if code[at + "ProceduralSkyMaterial".len()..].starts_with(".new") { out.push(finding(L, path, line_of(&code, at), SKY)); }
        }
        for gi in ["VoxelGI", "LightmapGI"] {
            for at in find_ident(&code, gi) {
                if code[at + gi.len()..].starts_with(".new") {
                    out.push(finding(L, path, line_of(&code, at), format!("{gi} is always off (L1850)")));
                }
            }
        }
        return out;
    }
    let sections = parse_resource(text);
    let file_type = sections.first().filter(|s| s.kind == "gd_resource").and_then(|s| s.type_name());
    for s in &sections {
        let ty = match s.kind.as_str() {
            "resource" => file_type.clone(),
            "sub_resource" | "node" | "ext_resource" => s.type_name(),
            _ => None,
        };
        let Some(ty) = ty else { continue };
        if matches!(s.kind.as_str(), "resource" | "sub_resource") && (ty == "Environment" || ty.starts_with("CameraAttributes")) {
            let get = |k: &str| s.prop(k).map(|p| (p.line, p.value.clone()));
            let glow_on = s.prop("glow_enabled").and_then(|p| parse_bool(&p.value));
            out.extend(check_env_values(path, &get, glow_on, s.line));
        }
        if ty == "ProceduralSkyMaterial" { out.push(finding(L, path, s.line, SKY)); }
        if s.kind == "node" && (ty == "VoxelGI" || ty == "LightmapGI") {
            out.push(finding(L, path, s.line, format!("{ty} node; VoxelGI and LightmapGI are always off (L1850)")));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// camera_far
// ------------------------------------------------------------------------------------------------

const MIN_FAR_M: f64 = 20_000.0;
/// Godot's `Camera3D.far` default, which applies when a scene omits the property.
const DEFAULT_FAR_M: f64 = 4_000.0;

/// `Camera3D` in world scenes with `far` < 20000. Unit: `xt-lint-godot`.
pub fn camera_far(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["tscn", "gd"]) {
        if !is_world_path(f) { continue; }
        out.extend(check_camera_far(f, &ctx.read(f)));
    }
    out
}

fn check_camera_far(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.camera-far";
    let mut out = Vec::new();
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        for (ln, v) in gd_assignments(&code, "far") {
            if let Some(n) = parse_num(&v) {
                if n < MIN_FAR_M { out.push(finding(L, path, ln, format!("camera far {n} m; world cameras see at least 20,000 m (L1873)"))); }
            }
        }
        return out;
    }
    for n in scene_nodes(&parse_resource(text)) {
        if n.ty != "Camera3D" { continue; }
        let far = n.prop_val("far").and_then(parse_num).unwrap_or(DEFAULT_FAR_M);
        if far < MIN_FAR_M {
            let line = n.prop("far").map_or(n.line, |p| p.line);
            out.push(finding(L, path, line, format!("Camera3D {} far is {far} m; world cameras need far >= 20,000 m (L1873)", n.path)));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// particles_local_coords
// ------------------------------------------------------------------------------------------------

/// `GPUParticles3D` with `local_coords = false`. Unit: `xt-lint-godot`.
pub fn particles_local_coords(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["tscn", "gd"]) {
        if exempt(f) { continue; }
        out.extend(check_particles(f, &ctx.read(f)));
    }
    out
}

fn check_particles(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.particles-local-coords";
    let mut out = Vec::new();
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        let sets = gd_assignments(&code, "local_coords");
        for (ln, v) in &sets {
            if v == "false" { out.push(finding(L, path, *ln, "local_coords = false; particles use local_coords true (L1951)")); }
        }
        let sets_true = sets.iter().any(|(_, v)| v == "true");
        for at in find_ident(&code, "GPUParticles3D") {
            if code[at + "GPUParticles3D".len()..].starts_with(".new") && !sets_true {
                out.push(finding(L, path, line_of(&code, at), "GPUParticles3D created without local_coords = true (the default is false; L1951)"));
            }
        }
        return out;
    }
    for n in scene_nodes(&parse_resource(text)) {
        if n.ty != "GPUParticles3D" { continue; }
        match n.prop("local_coords") {
            Some(p) if parse_bool(&p.value) == Some(true) => {}
            Some(p) => out.push(finding(L, path, p.line, format!("GPUParticles3D {} has local_coords = {}; it must be true (L1951)", n.path, p.value))),
            None => out.push(finding(L, path, n.line, format!("GPUParticles3D {} leaves local_coords at its default, false; set it true (L1951)", n.path))),
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// shader_hints
// ------------------------------------------------------------------------------------------------

const BANNED_HINTS: &[&str] = &["hint_depth_texture", "hint_screen_texture", "hint_normal_roughness_texture"];

/// Water and VFX shaders: under `game/shaders/water/` or `game/shaders/vfx/`, or any shader under `game/`
/// whose file name starts with `water` or `vfx`.
fn water_or_vfx(path: &str) -> bool {
    if path.starts_with("game/shaders/water/") || path.starts_with("game/shaders/vfx/") { return true; }
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with("water") || name.starts_with("vfx")
}

/// `#include "res://..."` targets of a shader, as repo paths.
fn shader_includes(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in code.lines() {
        if let Some(rest) = line.trim().strip_prefix("#include") {
            if let Some(p) = res_to_repo(&unquote(rest.trim())) { out.push(p); }
        }
    }
    out
}

/// `hint_depth_texture`, `hint_screen_texture` or `hint_normal_roughness_texture` in water or VFX shaders.
/// Unit: `xt-lint-godot`.
pub fn shader_hints(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["gdshader", "gdshaderinc"]) {
        if exempt(f) || !water_or_vfx(f) { continue; }
        // Follow includes so a hint cannot hide in a shared include.
        let mut seen: Vec<String> = Vec::new();
        let mut stack = vec![f.to_string()];
        while let Some(p) = stack.pop() {
            if seen.contains(&p) || seen.len() > 64 { continue; }
            seen.push(p.clone());
            let code = strip_c_comments(&ctx.read(&p));
            for (ln, hint) in check_shader_hints_code(&code) {
                let (line, via) = if p == f { (ln, String::new()) } else { (0, format!(" (via {p})")) };
                out.push(finding("godot.shader-hints", f, line, format!("{hint}{via}; water and VFX shaders make no depth, screen or normal reads (L1905)")));
            }
            stack.extend(shader_includes(&code));
        }
    }
    out
}

fn check_shader_hints_code(code: &str) -> Vec<(u32, &'static str)> {
    let mut out = Vec::new();
    for h in BANNED_HINTS {
        for at in find_ident(code, h) { out.push((line_of(code, at), *h)); }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// post_strings
// ------------------------------------------------------------------------------------------------

const POST_WORDS: &[&str] = &["chromatic", "grain", "lens_dirt", "lens_flare", "vignette"];

/// The strings `chromatic`, `grain`, `lens_dirt`, `lens_flare`, `vignette` in scenes, resources, shaders and
/// scripts under `game/` (whole-word, case-insensitive). Unit: `xt-lint-godot`.
pub fn post_strings(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["tscn", "tres", "gdshader", "gdshaderinc", "gd", "godot"]) {
        if exempt(f) { continue; }
        out.extend(check_post_strings(f, &ctx.read(f)));
    }
    out
}

/// Comments are not code: a shader may say why it has no vignette.
fn check_post_strings(path: &str, text: &str) -> Vec<Finding> {
    let code = if path.ends_with(".gd") {
        strip_gd_comments(text)
    } else if path.ends_with(".gdshader") || path.ends_with(".gdshaderinc") {
        strip_c_comments(text)
    } else {
        text.to_string()
    };
    let mut out = Vec::new();
    for w in POST_WORDS {
        for at in find_word_ci(&code, w) {
            out.push(finding("godot.post-strings", path, line_of(&code, at), format!("\"{w}\": no chromatic aberration, grain, lens dirt, lens flare or vignette (L1822, L2061)")));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// world_colour_literals
// ------------------------------------------------------------------------------------------------

/// World colour scope: shaders under `game/shaders/`, and scenes, resources and scripts under `game/world/`
/// and `game/art/`. The UI colour lint covers the rest of `game/`.
pub(super) fn is_world_colour_scope(path: &str) -> bool {
    path.starts_with("game/shaders/") || path.starts_with("game/world/") || path.starts_with("game/art/")
}

/// Identity multipliers are not colours: opaque white and transparent black.
fn identity_colour(args: &[f64]) -> bool {
    matches!(args, [1.0, 1.0, 1.0] | [1.0, 1.0, 1.0, 1.0] | [0.0, 0.0, 0.0, 0.0])
}

const SHADER_COLOUR_OUTPUTS: &[&str] = &["ALBEDO", "EMISSION", "COLOR", "FOG", "BACKLIGHT", "DIFFUSE_LIGHT", "SPECULAR_LIGHT", "AMBIENT_LIGHT", "SKY_COLOR", "GROUND_COLOR"];
const COLOUR_NAME_PARTS: &[&str] = &["col", "albedo", "tint", "emission", "emissive", "sky", "fog", "sun", "ambient", "paint", "rgb", "hue", "glow", "fire", "smoke", "foam", "shade", "dye"];

fn colourish_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    SHADER_COLOUR_OUTPUTS.contains(&name) || COLOUR_NAME_PARTS.iter().any(|p| l.contains(p))
}

/// Every `vec3(` / `vec4(` literal with only numeric arguments in a line, as (byte offset, args).
fn vec_literals(line: &str) -> Vec<(usize, Vec<f64>)> {
    let mut out = Vec::new();
    for ctor in ["vec3", "vec4"] {
        for at in find_ident(line, ctor) {
            if let Some(args) = ctor_args(&line[at..], ctor) { out.push((at, args)); }
        }
    }
    out
}

/// Shader line check: a numeric `vec3`/`vec4` constant that sets a colour (assigned to a colour output or a
/// colour-named variable, or a `source_color` uniform default). Splats of 0 or 1 (`vec3(0.0)`) are maths.
fn shader_colour_line(line: &str) -> Option<String> {
    let lits: Vec<(usize, Vec<f64>)> = vec_literals(line).into_iter().filter(|(_, a)| {
        let splat01 = a.len() == 1 && (a[0] == 0.0 || a[0] == 1.0);
        !a.is_empty() && !splat01 && !identity_colour(a)
    }).collect();
    if lits.is_empty() { return None; }
    if line.contains("source_color") { return Some("source_color uniform with a literal default".into()); }
    // The assigned name: the identifier before the first `=` (not `==`, `!=`, `<=`, `>=`) left of a literal.
    let b = line.as_bytes();
    let eq = (0..b.len()).find(|&i| b[i] == b'=' && b.get(i + 1) != Some(&b'=') && (i == 0 || !matches!(b[i - 1], b'=' | b'!' | b'<' | b'>')))?;
    if !lits.iter().any(|(at, _)| *at > eq) { return None; }
    let lhs = line[..eq].trim_end_matches(['+', '-', '*', '/']).trim();
    let name = lhs.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|s| !s.is_empty()).last()?;
    colourish_name(name).then(|| format!("colour constant assigned to {name}"))
}

/// A quoted `"#rgb"`, `"#rgba"`, `"#rrggbb"` or `"#rrggbbaa"` string in a line.
pub(super) fn hex_colour_at(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'"' && b.get(i + 1) == Some(&b'#') {
            let mut n = 0;
            while i + 2 + n < b.len() && (b[i + 2 + n] as char).is_ascii_hexdigit() { n += 1; }
            if matches!(n, 3 | 4 | 6 | 8) && b.get(i + 2 + n) == Some(&b'"') { return Some(i); }
        }
    }
    None
}

/// `Color(`, `vec3(` colour constants and hex colours in world shaders and world scenes; world colours come
/// from `game/art/tokens/world_tokens.json` or generated LUTs and ramps. Unit: `xt-lint-godot`.
pub fn world_colour_literals(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["gdshader", "gdshaderinc", "tscn", "tres", "gd"]) {
        if exempt(f) || !is_world_colour_scope(f) { continue; }
        out.extend(check_world_colours(f, &ctx.read(f)));
    }
    out
}

fn check_world_colours(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.world-colour-literals";
    const WHY: &str = "world colours come from game/art/tokens/world_tokens.json or generated LUTs and ramps (L1788)";
    const CTORS: &[&str] = &[".html(", ".hex(", ".hex64(", ".from_hsv(", ".from_string(", ".from_rgba8(", ".from_ok_hsl(", ".from_rgbe9995("];
    let mut out = Vec::new();
    let shader = path.ends_with(".gdshader") || path.ends_with(".gdshaderinc");
    let code = if shader { strip_c_comments(text) } else if path.ends_with(".gd") { strip_gd_comments(text) } else { text.to_string() };
    for (i, line) in code.lines().enumerate() {
        let ln = (i + 1) as u32;
        if shader {
            if let Some(why) = shader_colour_line(line) { out.push(finding(L, path, ln, format!("{why}; {WHY}"))); }
            continue;
        }
        for ctor in ["Color", "Color8"] {
            for at in find_ident(line, ctor) {
                let rest = &line[at..];
                let after = &rest[ctor.len()..];
                match ctor_args(rest, ctor) {
                    Some(args) if ctor == "Color" && identity_colour(&args) => {}
                    Some(args) if !args.is_empty() => out.push(finding(L, path, ln, format!("{ctor} literal; {WHY}"))),
                    _ if ctor == "Color8" && after.trim_start().starts_with('(') => out.push(finding(L, path, ln, format!("Color8 call; {WHY}"))),
                    _ => {
                        if CTORS.iter().any(|c| after.starts_with(c)) {
                            out.push(finding(L, path, ln, format!("Color constructor; {WHY}")));
                        } else if let Some(named) = after.strip_prefix('.') {
                            let name: String = named.chars().take_while(|c| c.is_ascii_uppercase() || *c == '_').collect();
                            let whole = named[name.len()..].chars().next().map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
                            if name.len() > 1 && whole && name != "WHITE" { out.push(finding(L, path, ln, format!("Color.{name}; {WHY}"))); }
                        }
                    }
                }
            }
        }
        if hex_colour_at(line).is_some() { out.push(finding(L, path, ln, format!("hex colour; {WHY}"))); }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// fonts
// ------------------------------------------------------------------------------------------------

const FONT_EXTS: &[&str] = &["ttf", "otf", "woff", "woff2", "ttc", "otc", "fnt", "pfb", "pfm"];

/// Whether a font file name is one of L1645's families: Atkinson Hyperlegible Next, Atkinson Hyperlegible Mono,
/// Noto Sans (Cyrillic), Noto Sans SC, Noto Sans JP.
fn allowed_font(file: &str) -> bool {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.split('.').next().unwrap_or(stem);
    let norm: String = stem.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    if norm.starts_with("atkinsonhyperlegiblenext") || norm.starts_with("atkinsonhyperlegiblemono") { return true; }
    if let Some(rest) = norm.strip_prefix("notosans") {
        const STYLE: &[&str] = &["regular", "bold", "variable", "vf", "wght", "wdth", "medium", "light", "semibold"];
        let rest = rest.strip_prefix("sc").or_else(|| rest.strip_prefix("jp")).unwrap_or(rest);
        return rest.is_empty() || STYLE.iter().any(|s| rest.starts_with(s));
    }
    false
}

/// Font files (`.ttf`, `.otf`, `.woff2`) other than Atkinson Hyperlegible Next, Atkinson Hyperlegible Mono and
/// the listed Noto fallbacks (L1645). Unit: `xt-lint-godot`.
pub fn fonts(ctx: &LintCtx) -> Vec<Finding> {
    const L: &str = "godot.fonts";
    let mut out = Vec::new();
    for f in ctx.files("", FONT_EXTS) {
        if !allowed_font(f) {
            out.push(finding(L, f, 0, "font file outside the listed families (Atkinson Hyperlegible Next and Mono, Noto Sans, Noto Sans SC, Noto Sans JP; L1605, L1645)"));
        }
    }
    for f in ctx.files("game/", &["tscn", "tres"]) {
        if exempt(f) { continue; }
        out.extend(check_font_refs(f, &ctx.read(f)));
    }
    out
}

fn check_font_refs(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.fonts";
    let mut out = Vec::new();
    let sections = parse_resource(text);
    let file_type = sections.first().filter(|s| s.kind == "gd_resource").and_then(|s| s.type_name());
    for s in &sections {
        let ty = match s.kind.as_str() { "resource" => file_type.clone(), "sub_resource" => s.type_name(), _ => None };
        if ty.as_deref() == Some("SystemFont") {
            out.push(finding(L, path, s.line, "SystemFont draws an unlisted OS font; use the bundled Atkinson and Noto files (L1645)"));
        }
        if s.kind == "ext_resource" {
            if let Some(p) = s.attr_str("path") {
                let ext = p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                if FONT_EXTS.contains(&ext.as_str()) && !allowed_font(&p) {
                    out.push(finding(L, path, s.line, format!("references the unlisted font {p} (L1645)")));
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// icons
// ------------------------------------------------------------------------------------------------

const ICON_DIR: &str = "res://ui/icons/";
/// Where UI textures may come from besides the icon set: generated art, and the placeholder quarantine
/// (which the release-only placeholder lint catches).
const TEXTURE_DIRS: &[&str] = &["res://ui/icons/", "res://art/gen/", "res://_placeholder/"];
const IMAGE_EXTS: &[&str] = &["svg", "png", "webp", "jpg", "jpeg", "tga", "bmp", "exr", "hdr", "ktx", "dds"];

/// Every `res://...` path in a text, with its byte offset.
fn res_paths(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("res://") {
        let at = from + rel;
        let end = text[at..].find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace()).map_or(text.len(), |e| at + e);
        out.push((at, text[at..end].to_string()));
        from = end.max(at + 6);
    }
    out
}

/// `.svg` or icon textures referenced from outside `res://ui/icons/`. Unit: `xt-lint-godot`.
pub fn icons(ctx: &LintCtx) -> Vec<Finding> {
    const L: &str = "godot.icons";
    let mut out = Vec::new();
    for f in ctx.files("game/", &["svg"]) {
        if exempt(f) || f.starts_with("game/ui/icons/") || f.starts_with("game/_placeholder/") { continue; }
        out.push(finding(L, f, 0, "SVG outside res://ui/icons/; icons are one hand-written set (L1606)"));
    }
    for f in ctx.files("game/", &["tscn", "tres", "gd"]) {
        if exempt(f) { continue; }
        out.extend(check_icon_refs(f, &ctx.read(f)));
    }
    out
}

fn check_icon_refs(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "godot.icons";
    let code = if path.ends_with(".gd") { strip_gd_comments(text) } else { text.to_string() };
    let ui = path.starts_with("game/ui/");
    let mut out = Vec::new();
    for (at, p) in res_paths(&code) {
        let ext = p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if ext == "svg" && !p.starts_with(ICON_DIR) && !p.starts_with("res://_placeholder/") {
            out.push(finding(L, path, line_of(&code, at), format!("{p}: SVG icons come only from {ICON_DIR} (L1606)")));
        } else if ui && ext != "svg" && IMAGE_EXTS.contains(&ext.as_str()) && !TEXTURE_DIRS.iter().any(|d| p.starts_with(d)) {
            out.push(finding(L, path, line_of(&code, at), format!("{p}: UI textures come only from {ICON_DIR} or generated art (L1606)")));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// placeholders
// ------------------------------------------------------------------------------------------------

/// Any `res://_placeholder/` reference in scenes, resources, scripts or Rust sources (release only).
/// Unit: `xt-lint-godot`.
pub fn placeholders(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ctx.files("game/", &["tscn", "tres", "gd", "gdshader", "gdshaderinc", "godot", "gdextension", "cfg", "json"]) {
        // The export presets name the folder to exclude it; files inside the quarantine may reference each other.
        if exempt(f) || f.starts_with("game/_placeholder/") || f == "game/export_presets.cfg" { continue; }
        out.extend(check_placeholders(f, &ctx.read(f)));
    }
    for f in ctx.files("crates/", &["rs"]) {
        if f.starts_with("crates/xtask/") { continue; }
        out.extend(check_placeholders(f, &ctx.read(f)));
    }
    out
}

fn check_placeholders(path: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find("res://_placeholder") {
        let at = from + rel;
        out.push(finding("godot.placeholders", path, line_of(text, at), "references res://_placeholder/; temporary art never ships (L1613, L1741)"));
        from = at + 1;
    }
    out
}

// ------------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------------

/// Text CSVs ship as raw files. The `I18n` autoload reads `res://i18n/*.csv` itself at start-up, and an export
/// leaves out the source of any file the editor imports, so each CSV's `.import` must use the `keep` importer
/// ("Keep file (exported as is)"); a CSV the editor imports as a translation is missing from the release
/// build, and every string in it shows as its key. Unit: `xt-lint-godot` (release packaging).
pub fn i18n_keep(ctx: &LintCtx) -> Vec<Finding> {
    const L: &str = "godot.i18n-keep";
    let mut out = Vec::new();
    for f in ctx.files("game/i18n/", &["csv"]) {
        let import = format!("{f}.import");
        let text = ctx.read(&import);
        if !text.lines().any(|l| l.trim() == "importer=\"keep\"") {
            out.push(finding(L, &import, 0, "i18n CSVs use importer=\"keep\" so exports ship the raw file; write [remap] importer=\"keep\""));
        }
    }
    for f in ctx.files("game/i18n/", &["translation"]) {
        out.push(finding(L, f, 0, "imported translation beside an i18n CSV; the CSVs are kept as is, delete this file"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_PROJECT: &str = "config_version=5\n\n[physics]\n\n3d/physics_engine=\"Dummy\"\n\n[rendering]\n\nrenderer/rendering_method=\"forward_plus\"\nrendering_device/driver.windows=\"d3d12\"\nrendering_device/driver.macos=\"metal\"\nrendering_device/driver.linuxbsd=\"vulkan\"\nrendering_device/fallback_to_vulkan=true\nrendering_device/fallback_to_d3d12=true\nrendering_device/fallback_to_opengl3=true\n";

    #[test]
    fn i18n_csvs_must_keep_their_source() {
        let root = std::env::temp_dir().join(format!("bp-i18n-keep-{}", std::process::id()));
        let dir = root.join("game/i18n");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.csv"), "keys,?context,en\n").unwrap();
        std::fs::write(dir.join("a.csv.import"), "[remap]\n\nimporter=\"keep\"\n").unwrap();
        std::fs::write(dir.join("b.csv"), "keys,?context,en\n").unwrap();
        std::fs::write(dir.join("b.csv.import"), "[remap]\n\nimporter=\"csv_translation\"\n").unwrap();
        std::fs::write(dir.join("c.csv"), "keys,?context,en\n").unwrap();
        std::fs::write(dir.join("b.en.translation"), "x").unwrap();
        let ctx = LintCtx::new(&root, super::super::Profile::Pr);
        let found: Vec<String> = i18n_keep(&ctx).into_iter().map(|f| f.path).collect();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found, vec!["game/i18n/b.csv.import", "game/i18n/c.csv.import", "game/i18n/b.en.translation"]);
    }

    #[test]
    fn renderer_keys_pass_and_fail() {
        assert!(check_renderer_keys("p", GOOD_PROJECT).is_empty());
        let crlf = GOOD_PROJECT.replace('\n', "\r\n");
        assert!(check_renderer_keys("p", &crlf).is_empty());
        let changed = GOOD_PROJECT.replace("\"d3d12\"", "\"vulkan\"");
        assert_eq!(check_renderer_keys("p", &changed).len(), 1);
        let missing = GOOD_PROJECT.replace("rendering_device/fallback_to_opengl3=true\n", "");
        assert_eq!(check_renderer_keys("p", &missing).len(), 1);
        let jolt = GOOD_PROJECT.replace("\"Dummy\"", "\"Jolt Physics\"");
        assert_eq!(check_renderer_keys("p", &jolt).len(), 1);
        let mobile = format!("{GOOD_PROJECT}renderer/rendering_method.mobile=\"mobile\"\n");
        assert_eq!(check_renderer_keys("p", &mobile).len(), 1);
    }

    #[test]
    fn resource_parser_handles_multiline_values_and_paths() {
        let t = "[gd_scene format=3]\n\n[node name=\"Root\" type=\"Control\"]\nx = {\n\"a\": 1\n}\n\n[node name=\"A\" type=\"VBoxContainer\" parent=\".\"]\n\n[node name=\"B\" type=\"Label\" parent=\"A\"]\ntext = \"hi = there\"\n";
        let s = parse_resource(t);
        assert_eq!(s.len(), 4);
        assert_eq!(s[1].props[0].value, "{\n\"a\": 1\n}");
        let nodes = scene_nodes(&s);
        assert_eq!(nodes.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(), vec![".", "A", "A/B"]);
        assert_eq!(unquote(nodes[2].prop_val("text").unwrap()), "hi = there");
        assert_eq!(parent_path(&nodes[2]).as_deref(), Some("A"));
        assert_eq!(resource_refs("{&\"\": SubResource(\"lib_1\"), &\"b\": SubResource(\"lib_2\")}", "SubResource"), vec!["lib_1", "lib_2"]);
    }

    #[test]
    fn physics_bodies_in_scenes_and_scripts() {
        let t = "[gd_scene format=3]\n[node name=\"R\" type=\"Node3D\"]\n[node name=\"B\" type=\"RigidBody3D\" parent=\".\"]\n";
        assert_eq!(check_physics_bodies("game/world/x.tscn", t).len(), 1);
        assert_eq!(check_physics_bodies("game/world/x.gd", "extends CharacterBody3D\n").len(), 1);
        assert!(check_physics_bodies("game/world/x.gd", "# a RigidBody3D.new() in a comment\nvar n := 1\n").is_empty());
        assert!(!is_world_path("game/ui/hud/x.tscn"));
        assert!(!is_world_path("game/tools/x.tscn"));
        assert!(is_world_path("game/world/x.tscn"));
    }

    #[test]
    fn environment_rules() {
        let good = "[gd_resource type=\"Environment\" format=3]\n[resource]\nbackground_mode = 2\nglow_enabled = true\nglow_blend_mode = 1\nglow_hdr_threshold = 1.0\nfog_height_density = 0.0\n";
        assert!(check_environment("game/world/env.tres", good).is_empty());
        let bad = "[gd_scene format=3]\n[sub_resource type=\"ProceduralSkyMaterial\" id=\"s\"]\n[sub_resource type=\"Environment\" id=\"e\"]\nbackground_mode = 3\nglow_enabled = true\nglow_hdr_threshold = 0.8\nglow_bloom = 0.1\nfog_height_density = 0.2\nfog_aerial_perspective = 0.5\nssil_enabled = true\nsdfgi_enabled = true\n[sub_resource type=\"CameraAttributesPractical\" id=\"c\"]\nauto_exposure_enabled = true\n[node name=\"R\" type=\"Node3D\"]\n[node name=\"G\" type=\"VoxelGI\" parent=\".\"]\n";
        let f = check_environment("game/world/w.tscn", bad);
        // sky, canvas, blend mode unset, threshold, bloom, height density, aerial (default density), ssil, sdfgi,
        // auto exposure, VoxelGI.
        assert_eq!(f.len(), 11, "{f:#?}");
        let thin_fog = "[gd_resource type=\"Environment\" format=3]\n[resource]\nfog_density = 0.0001\nfog_aerial_perspective = 0.5\n";
        assert!(check_environment("game/world/e.tres", thin_fog).is_empty());
        let script = "func apply(env: Environment) -> void:\n\tenv.glow_blend_mode = Environment.GLOW_BLEND_MODE_SOFTLIGHT\n\tenv.glow_hdr_threshold = 0.9\n\tenv.set(\"fog_height_density\", 0.1)\n\tenv.background_mode = Environment.BG_SKY\n\tenv.glow_bloom = bloom_from_preset\n";
        let f = check_environment("game/world/environment.gd", script);
        assert_eq!(f.len(), 3, "{f:#?}");
    }

    #[test]
    fn camera_far_default_and_values() {
        let t = "[gd_scene format=3]\n[node name=\"R\" type=\"Node3D\"]\n[node name=\"A\" type=\"Camera3D\" parent=\".\"]\n[node name=\"B\" type=\"Camera3D\" parent=\".\"]\nfar = 20000.0\n[node name=\"C\" type=\"Camera3D\" parent=\".\"]\nfar = 5000.0\n";
        assert_eq!(check_camera_far("game/world/w.tscn", t).len(), 2);
        assert_eq!(check_camera_far("game/world/c.gd", "func _ready():\n\tcam.far = 4000.0\n\tcam.far = 30000.0\n").len(), 1);
    }

    #[test]
    fn particles_need_local_coords_true() {
        let t = "[gd_scene format=3]\n[node name=\"R\" type=\"Node3D\"]\n[node name=\"A\" type=\"GPUParticles3D\" parent=\".\"]\n[node name=\"B\" type=\"GPUParticles3D\" parent=\".\"]\nlocal_coords = true\n[node name=\"C\" type=\"GPUParticles3D\" parent=\".\"]\nlocal_coords = false\n";
        assert_eq!(check_particles("game/world/p.tscn", t).len(), 2);
        assert_eq!(check_particles("game/world/p.gd", "var p := GPUParticles3D.new()\n").len(), 1);
        assert!(check_particles("game/world/p.gd", "var p := GPUParticles3D.new()\np.local_coords = true\n").is_empty());
    }

    #[test]
    fn shader_hints_and_scope() {
        let code = strip_c_comments("shader_type spatial;\n// hint_screen_texture in a comment\nuniform sampler2D d : hint_depth_texture;\n#include \"res://shaders/gen/waves.gdshaderinc\"\n");
        assert_eq!(check_shader_hints_code(&code).len(), 1);
        assert_eq!(shader_includes(&code), vec!["game/shaders/gen/waves.gdshaderinc".to_string()]);
        assert!(water_or_vfx("game/shaders/water.gdshader"));
        assert!(water_or_vfx("game/shaders/vfx/spray.gdshader"));
        assert!(water_or_vfx("game/shaders/vfx_blend.gdshader"));
        assert!(!water_or_vfx("game/shaders/hull.gdshader"));
    }

    #[test]
    fn post_strings_words() {
        assert_eq!(check_post_strings("game/world/a.gd", "var film_grain := 1\nvar Vignette := 2\n# lens_flare in a comment\n").len(), 2);
        assert!(check_post_strings("game/world/a.gd", "var grains := 1\nvar ingrained := 2\n").is_empty());
        assert_eq!(check_post_strings("game/shaders/a.gdshader", "uniform float lens_dirt_amount;\n").len(), 1);
    }

    #[test]
    fn world_colours_in_shaders_and_scenes() {
        let sh = "shader_type spatial;\nuniform vec3 hull_tint : source_color = vec3(0.3, 0.32, 0.35);\nvoid fragment() {\n\tvec3 n = vec3(0.0, 1.0, 0.0);\n\tALBEDO = vec3(0.2, 0.1, 0.1);\n\tvec3 sun_col = vec3(1.0, 0.8, 0.6);\n\tALBEDO = vec3(0.0);\n\tEMISSION = albedo * vec3(1.0);\n\tvec3 offset = vec3(0.5, 0.25, 0.0);\n\tif (x == vec3(0.2, 0.3, 0.4).x) {}\n}\n";
        let f = check_world_colours("game/shaders/x.gdshader", sh);
        assert_eq!(f.iter().map(|f| f.line).collect::<Vec<_>>(), vec![2, 5, 6], "{f:#?}");
        let sc = "[gd_scene format=3]\n[node name=\"R\" type=\"Node3D\"]\nmodulate = Color(1, 1, 1, 1)\n[node name=\"L\" type=\"OmniLight3D\" parent=\".\"]\nlight_color = Color(1, 0.6, 0.3, 1)\n";
        assert_eq!(check_world_colours("game/world/x.tscn", sc).len(), 1);
        assert_eq!(check_world_colours("game/world/x.gd", "var c := Color.ORANGE\nvar h := \"#ff8800\"\nvar w := Color(0.1, 0.2, 0.3)\nvar k := Color(r, g, b)\nvar t: Color = Color.WHITE\n").len(), 3);
    }

    #[test]
    fn font_families() {
        assert!(allowed_font("game/ui/fonts/AtkinsonHyperlegibleNext-Variable.ttf"));
        assert!(allowed_font("game/ui/fonts/AtkinsonHyperlegibleMono-Regular.woff2"));
        assert!(allowed_font("game/ui/fonts/NotoSans-Regular.ttf"));
        assert!(allowed_font("game/ui/fonts/NotoSansSC-Bold.otf"));
        assert!(allowed_font("game/ui/fonts/NotoSansJP[wght].ttf"));
        assert!(!allowed_font("game/ui/fonts/Inter-Regular.ttf"));
        assert!(!allowed_font("game/ui/fonts/NotoSansMono-Regular.ttf"));
        assert!(!allowed_font("game/ui/fonts/AtkinsonHyperlegible-Regular.ttf"));
        assert!(!allowed_font("docs/fonts/OpenSans.woff2"));
        assert_eq!(check_font_refs("game/ui/x.tres", "[gd_resource type=\"SystemFont\" format=3]\n[resource]\nfont_names = PackedStringArray(\"Inter\")\n").len(), 1);
    }

    #[test]
    fn icon_references() {
        let t = "[gd_scene format=3]\n[ext_resource type=\"Texture2D\" path=\"res://ui/icons/add.svg\" id=\"1\"]\n[ext_resource type=\"Texture2D\" path=\"res://art/logo.svg\" id=\"2\"]\n[ext_resource type=\"Texture2D\" path=\"res://ui/hud/badge.png\" id=\"3\"]\n[ext_resource type=\"Texture2D\" path=\"res://art/gen/noise.png\" id=\"4\"]\n";
        assert_eq!(check_icon_refs("game/ui/hud/x.tscn", t).len(), 2);
        assert_eq!(check_icon_refs("game/world/x.gd", "var i = load(\"res://world/flag.svg\")\n").len(), 1);
    }

    #[test]
    fn placeholder_references() {
        assert_eq!(check_placeholders("game/world/x.tscn", "[ext_resource path=\"res://_placeholder/box.tres\" id=\"1\"]\n").len(), 1);
        assert!(check_placeholders("game/world/x.tscn", "[ext_resource path=\"res://art/box.tres\" id=\"1\"]\n").is_empty());
    }

    #[test]
    fn word_and_ident_search() {
        assert_eq!(find_word_ci("Grain, grain_x, ingrain", "grain"), vec![0, 7]);
        assert_eq!(find_ident("far = 1; self.far=2; farther", "far"), vec![0, 14]);
        assert_eq!(gd_assignments("cam.far = 10\nif cam.far == 3:\n", "far"), vec![(1, "10".to_string())]);
        assert_eq!(strip_gd_comments("a = \"#x\" # c\nb = '''\n# not\n'''\n"), "a = \"#x\" \nb = '''\n# not\n'''\n");
    }
}
