//! Unit `xt-lint-ui`. UI scene and script lints from "Interface and visual design" (L1561-1622, L1724-1745).
//! Scopes: `game/ui/**/*.tscn`, `*.tres`, `*.gd` and every other `*.tscn` / `*.gd` under `game/` for the
//! override, colour and tween rules. Owns `game/ui/lint/meta_fields.txt` (bound Meta label fields).
//!
//! These are the text-level twins of the engine-side rules in `game/test/lint/lint_rules.gd`; both read the
//! same lists (`meta_fields.txt`: `<scene path>#<node path> = <source>`, scene paths `res://` or `game/`
//! relative, node paths as Godot's `get_node_path` without `./`). `game/test/` and `game/addons/` are exempt.
use super::godot::{
    ctor_args, exempt, finding, find_ident, hex_colour_at, is_world_colour_scope, line_of, parent_path, parse_bool, parse_num, parse_resource, resource_refs,
    scene_nodes, strip_gd_comments, unquote, gd_assignments, Node, Section,
};
use super::{Finding, LintCtx};

const META_FIELDS: &str = "game/ui/lint/meta_fields.txt";
const THEME: &str = "game/ui/theme/project_theme.tres";
const SIM_SIGNAL_META: &str = "sim_signal";

fn ui_scope<'a>(ctx: &'a LintCtx, exts: &'a [&'a str]) -> impl Iterator<Item = &'a str> + 'a {
    ctx.files("game/", exts).filter(|f| !exempt(f))
}

fn is_label_type(ty: &str) -> bool { matches!(ty, "Label" | "RichTextLabel" | "LinkButton" | "Label3D") }
fn is_text_label(ty: &str) -> bool { matches!(ty, "Label" | "RichTextLabel") }
fn variation(n: &Node) -> String { n.prop_val("theme_type_variation").map(unquote).unwrap_or_default() }

/// Split a GDScript argument list at top-level commas.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q { quote = None; }
            continue;
        }
        match c {
            '"' | '\'' => { quote = Some(c); cur.push(c); }
            '(' | '[' | '{' => { depth += 1; cur.push(c); }
            ')' | ']' | '}' => {
                if depth == 0 { break; }
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => { out.push(cur.trim().to_string()); cur.clear(); }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() { out.push(cur.trim().to_string()); }
    out
}

/// `tween_property(obj, "prop", value, duration)` calls in a line: (target, property without sub-path, sub-path,
/// duration if numeric).
fn tween_props(line: &str) -> Vec<(String, String, String, Option<f64>)> {
    let mut out = Vec::new();
    for at in find_ident(line, "tween_property") {
        let rest = line[at + "tween_property".len()..].trim_start();
        let Some(args) = rest.strip_prefix('(') else { continue };
        let a = split_args(args);
        if a.len() < 2 { continue; }
        let p = unquote(&a[1]);
        let (prop, sub) = match p.split_once(':') { Some((x, y)) => (x.to_string(), y.to_string()), None => (p.clone(), String::new()) };
        out.push((a[0].clone(), prop, sub, a.get(3).and_then(|d| parse_num(d))));
    }
    out
}

/// GDScript functions as (first line index, last line index exclusive) over the stripped code's lines.
fn functions(lines: &[&str]) -> Vec<(usize, usize)> {
    let starts: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| {
        let t = l.trim_start();
        t.starts_with("func ") || t.starts_with("static func ")
    }).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        out.push((s, starts.get(k + 1).copied().unwrap_or(lines.len())));
    }
    out
}

// ------------------------------------------------------------------------------------------------
// theme_overrides
// ------------------------------------------------------------------------------------------------

const OVERRIDE_PROPS: &[&str] = &["theme_override_colors/", "theme_override_fonts/", "theme_override_font_sizes/", "theme_override_styles/"];
const OVERRIDE_CALLS: &[&str] = &["add_theme_color_override", "add_theme_font_override", "add_theme_font_size_override", "add_theme_stylebox_override"];

/// `theme_override_colors/`, `theme_override_fonts/`, `theme_override_font_sizes/`, `theme_override_styles/` in
/// `.tscn`; `add_theme_color_override(`, `add_theme_font_override(`, `add_theme_font_size_override(`,
/// `add_theme_stylebox_override(` in `.gd`. Constants overrides are allowed. Unit: `xt-lint-ui`.
pub fn theme_overrides(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["tscn", "gd"]) { out.extend(check_theme_overrides(f, &ctx.read(f))); }
    out
}

fn check_theme_overrides(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "ui.theme-overrides";
    let mut out = Vec::new();
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        for call in OVERRIDE_CALLS {
            for at in find_ident(&code, call) {
                if code[at + call.len()..].trim_start().starts_with('(') {
                    out.push(finding(L, path, line_of(&code, at), format!("{call}(); looks come from type variations (L1728)")));
                }
            }
        }
        for p in OVERRIDE_PROPS {
            let mut from = 0;
            while let Some(rel) = code[from..].find(p) {
                let at = from + rel;
                if code[..at].ends_with('"') || code[..at].ends_with('\'') {
                    out.push(finding(L, path, line_of(&code, at), format!("sets {p}* by name; looks come from type variations (L1728)")));
                }
                from = at + p.len();
            }
        }
        return out;
    }
    for s in parse_resource(text) {
        for p in &s.props {
            if OVERRIDE_PROPS.iter().any(|o| p.key.starts_with(o)) {
                let who = s.attr_str("name").unwrap_or_default();
                out.push(finding(L, path, p.line, format!("{who} sets {}; use a type variation (L1728)", p.key)));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// colour_literals
// ------------------------------------------------------------------------------------------------

/// Folders whose colours come from a token file or that ship no UI (matches `godot_patterns.txt`). World
/// scenes and shaders belong to `godot.world-colour-literals`.
fn colour_exempt(path: &str) -> bool {
    exempt(path) || path.starts_with("game/ui/theme/") || path.starts_with("game/_placeholder/") || is_world_colour_scope(path)
}

const COLOUR_FNS: &[&str] = &["html", "hex", "hex64", "from_hsv", "from_ok_hsl", "from_rgba8", "from_rgbe9995", "from_string"];

/// The first colour literal in a line, as a short description.
fn colour_in_line(line: &str) -> Option<String> {
    for at in find_ident(line, "Color") {
        let after = &line[at + 5..];
        let t = after.trim_start();
        if t.starts_with('(') { return Some("Color(...)".into()); }
        if let Some(named) = after.strip_prefix('.') {
            let ident: String = named.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if COLOUR_FNS.contains(&ident.as_str()) && named[ident.len()..].trim_start().starts_with('(') { return Some(format!("Color.{ident}()")); }
            if ident.len() > 1 && ident.chars().all(|c| c.is_ascii_uppercase() || c == '_') { return Some(format!("Color.{ident}")); }
        }
    }
    for at in find_ident(line, "Color8") {
        if line[at + 6..].trim_start().starts_with('(') { return Some("Color8(...)".into()); }
    }
    hex_colour_at(line).map(|_| "hex colour string".into())
}

/// `Color(`, `Color.html(`, `Color8(` and named `Color.RED`-style constants in `.gd`, `.tscn`, `.tres` outside
/// `game/ui/theme/` (the theme compiler reads tokens.json). Unit: `xt-lint-ui`.
pub fn colour_literals(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["gd", "tscn", "tres"]) {
        if colour_exempt(f) { continue; }
        out.extend(check_colour_literals(f, &ctx.read(f)));
    }
    out
}

fn check_colour_literals(path: &str, text: &str) -> Vec<Finding> {
    let code = if path.ends_with(".gd") { strip_gd_comments(text) } else { text.to_string() };
    code.lines().enumerate().filter_map(|(i, l)| colour_in_line(l).map(|what| {
        finding("ui.colour-literals", path, (i + 1) as u32, format!("{what}; colours come only from res://ui/theme/tokens.json through the theme (L1593)"))
    })).collect()
}

// ------------------------------------------------------------------------------------------------
// tweens
// ------------------------------------------------------------------------------------------------

const BANNED_TRANS: &[&str] = &["TRANS_ELASTIC", "TRANS_BOUNCE", "TRANS_BACK", "TRANS_SPRING"];
const BANNED_EASE: &[&str] = &["EASE_IN", "EASE_IN_OUT", "EASE_OUT_IN"];
const LAYOUT_PROPS: &[&str] = &[
    "position", "global_position", "size", "custom_minimum_size", "anchor_left", "anchor_top", "anchor_right", "anchor_bottom",
    "offset_left", "offset_top", "offset_right", "offset_bottom",
];

/// `TRANS_ELASTIC`, `TRANS_BOUNCE`, `TRANS_BACK`, `TRANS_SPRING`; tweens of `position`, `size` or anchors;
/// tweens or AnimationPlayers without sim-signal metadata (L1737). Unit: `xt-lint-ui`.
pub fn tweens(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["gd", "tscn", "tres"]) { out.extend(check_tweens(f, &ctx.read(f))); }
    out
}

fn check_tweens(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "ui.tween";
    let mut out = Vec::new();
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        let lines: Vec<&str> = code.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let ln = (i + 1) as u32;
            for t in BANNED_TRANS {
                if !find_ident(line, t).is_empty() { out.push(finding(L, path, ln, format!("{t} is banned; motion only reports state (L1737)"))); }
            }
            for e in BANNED_EASE {
                if !find_ident(line, e).is_empty() { out.push(finding(L, path, ln, format!("{e}; non-linear tweens use EASE_OUT (L1737)"))); }
            }
            for (_, prop, _, _) in tween_props(line) {
                if LAYOUT_PROPS.contains(&prop.as_str()) {
                    out.push(finding(L, path, ln, format!("tweens {prop}; animate offset_transform_* (visual only), never position, size or anchors (L1737)")));
                }
            }
        }
        for (s, e) in functions(&lines) {
            let body = &lines[s..e];
            let creates = body.iter().any(|l| l.contains("create_tween(") || l.contains("Tween.new("));
            let named = body.iter().any(|l| l.contains("set_meta(") && l.contains(SIM_SIGNAL_META));
            if creates && !named {
                out.push(finding(L, path, (s + 1) as u32, format!("a Tween is made without set_meta(\"{SIM_SIGNAL_META}\", ...) naming its sim signal (L1737)")));
            }
        }
        return out;
    }
    let sections = parse_resource(text);
    for n in scene_nodes(&sections) {
        if n.ty == "AnimationPlayer" && n.prop(&format!("metadata/{SIM_SIGNAL_META}")).is_none() {
            out.push(finding(L, path, n.line, format!("AnimationPlayer {} has no metadata/{SIM_SIGNAL_META} naming its sim signal (L1737)", n.path)));
        }
    }
    for s in sections.iter().filter(|s| is_animation(s, &sections)) {
        for (track_line, _target, prop) in animation_tracks(s) {
            if LAYOUT_PROPS.contains(&prop.as_str()) {
                out.push(finding(L, path, track_line, format!("animation track on {prop}; animate offset_transform_* only (L1737)")));
            }
        }
    }
    out
}

fn is_animation(s: &Section, all: &[Section]) -> bool {
    match s.kind.as_str() {
        "sub_resource" => s.type_name().as_deref() == Some("Animation"),
        "resource" => all.first().and_then(|h| h.type_name()).as_deref() == Some("Animation"),
        _ => false,
    }
}

/// Tracks of an Animation section: (line, node path, property without sub-path).
fn animation_tracks(s: &Section) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    for p in &s.props {
        if p.key.starts_with("tracks/") && p.key.ends_with("/path") {
            let v = p.value.trim();
            let inner = v.strip_prefix("NodePath(").and_then(|x| x.strip_suffix(')')).map(unquote).unwrap_or_else(|| unquote(v));
            if let Some((node, prop)) = inner.split_once(':') {
                out.push((p.line, node.to_string(), prop.split(':').next().unwrap_or("").to_string()));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// uppercase
// ------------------------------------------------------------------------------------------------

/// `uppercase = true` on any Label. Unit: `xt-lint-ui`.
pub fn uppercase(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["tscn", "tres", "gd"]) { out.extend(check_uppercase(f, &ctx.read(f))); }
    out
}

fn check_uppercase(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "ui.uppercase";
    const WHY: &str = "Label.uppercase stays false; sentence case, capitals only for codes (L1607, L1730)";
    if path.ends_with(".gd") {
        let code = strip_gd_comments(text);
        return gd_assignments(&code, "uppercase").into_iter().filter(|(_, v)| v == "true").map(|(ln, _)| finding(L, path, ln, WHY)).collect();
    }
    let mut out = Vec::new();
    for s in parse_resource(text) {
        if let Some(p) = s.prop("uppercase") {
            if parse_bool(&p.value) == Some(true) { out.push(finding(L, path, p.line, WHY)); }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// eyebrow
// ------------------------------------------------------------------------------------------------

const EYEBROW_NAMES: &[&str] = &["Eyebrow", "Kicker", "Overline", "Tagline", "SectionLabel", "Pretitle"];
const TITLE_VARIATIONS: &[&str] = &["ScreenTitle", "PanelTitle", "MenuTitle"];
const META_VARIATION: &str = "Meta";

/// `meta_fields.txt` keys (`scene#node`, scene path without `res://` or `game/`).
fn read_meta_fields(text: &str) -> Vec<(String, String, u32)> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let key = key.strip_prefix("res://").or_else(|| key.strip_prefix("game/")).unwrap_or(key).to_string();
        out.push((key, line[eq + 1..].trim().to_string(), (i + 1) as u32));
    }
    out
}

fn scene_key(path: &str) -> String { path.strip_prefix("game/").unwrap_or(path).to_string() }

fn eyebrow_name(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    EYEBROW_NAMES.iter().copied().find(|b| l.contains(&b.to_ascii_lowercase()))
}

fn stacked_vertically(n: &Node) -> bool {
    matches!(n.ty.as_str(), "VBoxContainer" | "VFlowContainer") || (n.ty == "BoxContainer" && n.prop_val("vertical").and_then(parse_bool) == Some(true))
}

fn is_title(n: &Node) -> bool { is_text_label(&n.ty) && TITLE_VARIATIONS.contains(&variation(n).as_str()) }

fn children<'a>(nodes: &'a [Node], parent: &str) -> Vec<&'a Node> {
    nodes.iter().filter(|n| parent_path(n).as_deref() == Some(parent)).collect()
}

/// A non-title label, or a container holding one or two such labels (a pill).
fn only_labels(nodes: &[Node], n: &Node, depth: u32) -> bool {
    if is_text_label(&n.ty) { return !is_title(n); }
    if depth > 4 || !n.ty.ends_with("Container") { return false; }
    let kids = children(nodes, &n.path);
    !kids.is_empty() && kids.len() <= 2 && kids.iter().all(|k| only_labels(nodes, k, depth + 1))
}

/// A Label immediately before a title Label (ScreenTitle, PanelTitle, MenuTitle variations); a Meta Label not
/// bound to a field in `game/ui/lint/meta_fields.txt`; more than ceil(panels / 4) Meta lines per screen; nodes
/// named Eyebrow, Kicker, Overline, Tagline, SectionLabel or Pretitle. Unit: `xt-lint-ui`.
pub fn eyebrow(ctx: &LintCtx) -> Vec<Finding> {
    let meta_text = ctx.read(META_FIELDS);
    let meta = read_meta_fields(&meta_text);
    let keys: Vec<String> = meta.iter().map(|(k, _, _)| k.clone()).collect();
    let mut out = Vec::new();
    let mut scenes: Vec<(String, Vec<Node>)> = Vec::new();
    for f in ui_scope(ctx, &["tscn", "gd"]) {
        let text = ctx.read(f);
        if f.ends_with(".gd") {
            out.extend(check_eyebrow_script(f, &text));
            continue;
        }
        out.extend(check_eyebrow_scene(f, &text, &keys));
        scenes.push((scene_key(f), scene_nodes(&parse_resource(&text))));
    }
    // Stale bindings: the scene or node is gone, or the node is not a Meta label.
    for (key, source, ln) in &meta {
        let (scene, node) = key.split_once('#').unwrap_or((key.as_str(), ""));
        let found = scenes.iter().find(|(s, _)| s == scene).and_then(|(_, nodes)| nodes.iter().find(|n| n.path == node));
        match found {
            None => out.push(finding("ui.eyebrow", META_FIELDS, *ln, format!("{key} = {source}: no such scene node"))),
            Some(n) if !(is_text_label(&n.ty) && variation(n) == META_VARIATION) => {
                out.push(finding("ui.eyebrow", META_FIELDS, *ln, format!("{key} is not a Meta label")))
            }
            _ if source.is_empty() => out.push(finding("ui.eyebrow", META_FIELDS, *ln, format!("{key} names no data source"))),
            _ => {}
        }
    }
    out
}

fn check_eyebrow_script(path: &str, text: &str) -> Vec<Finding> {
    let code = strip_gd_comments(text);
    let mut out = Vec::new();
    for (i, line) in code.lines().enumerate() {
        if let Some(rest) = line.trim_start().strip_prefix("class_name") {
            let name = rest.trim().split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
            if let Some(b) = eyebrow_name(name) {
                out.push(finding("ui.eyebrow", path, (i + 1) as u32, format!("class {name} is named like an eyebrow ({b}; L1578)")));
            }
        }
    }
    out
}

fn check_eyebrow_scene(path: &str, text: &str, meta_keys: &[String]) -> Vec<Finding> {
    const L: &str = "ui.eyebrow";
    let nodes = scene_nodes(&parse_resource(text));
    let skey = scene_key(path);
    let mut out = Vec::new();
    let mut panels = 0usize;
    let mut metas: Vec<&Node> = Vec::new();
    for n in &nodes {
        if let Some(b) = eyebrow_name(&n.name) {
            out.push(finding(L, path, n.line, format!("node {} is named like an eyebrow ({b}); put facts in the heading (L1578)", n.path)));
        }
        let var = variation(n);
        if let Some(b) = eyebrow_name(&var).or_else(|| eyebrow_name(&n.ty)) {
            out.push(finding(L, path, n.line, format!("{} uses an eyebrow-named type ({b}; L1578)", n.path)));
        }
        if matches!(n.ty.as_str(), "Panel" | "PanelContainer") { panels += 1; }
        if is_text_label(&n.ty) && var == META_VARIATION { metas.push(n); }
        if is_title(n) {
            let Some(pp) = parent_path(n) else { continue };
            let Some(parent) = nodes.iter().find(|p| p.path == pp) else { continue };
            if !stacked_vertically(parent) { continue; }
            let sibs = children(&nodes, &pp);
            let Some(at) = sibs.iter().position(|s| s.path == n.path) else { continue };
            if at == 0 { continue; }
            let before = sibs[at - 1];
            if only_labels(&nodes, before, 0) {
                out.push(finding(L, path, before.line, format!("{} sits directly above the title {}; put facts in the heading or a bound Meta line after it (L1578)", before.path, n.path)));
            }
        }
    }
    for m in &metas {
        let key = format!("{skey}#{}", m.path);
        if !meta_keys.contains(&key) {
            out.push(finding(L, path, m.line, format!("Meta label {} is not bound in res://ui/lint/meta_fields.txt ({key}; L1578)", m.path)));
        }
    }
    let allowed = panels.max(1).div_ceil(4);
    if metas.len() > allowed {
        out.push(finding(L, path, 0, format!("{} Meta lines for {panels} panels; at most ceil(panels / 4) = {allowed} (L1578)", metas.len())));
    }
    out
}

// ------------------------------------------------------------------------------------------------
// glass
// ------------------------------------------------------------------------------------------------

/// Scrims (pause and radial menus) may dim and blur what is under them (the spec's exception).
fn scrim(s: &str) -> bool { s.to_ascii_lowercase().contains("scrim") }

fn blur_shader_code(code: &str) -> bool {
    let canvas = code.contains("shader_type canvas_item");
    canvas && !find_ident(code, "hint_screen_texture").is_empty() && !find_ident(code, "textureLod").is_empty()
}

/// `BackBufferCopy` nodes and blur shaders (`textureLod` on screen texture, `blur` in shader names).
/// Unit: `xt-lint-ui`.
pub fn glass(ctx: &LintCtx) -> Vec<Finding> {
    const L: &str = "ui.glass";
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["tscn", "tres"]) { out.extend(check_glass_scene(f, &ctx.read(f))); }
    for f in ui_scope(ctx, &["gdshader", "gdshaderinc"]) {
        if scrim(f) { continue; }
        let name = f.rsplit('/').next().unwrap_or(f).to_ascii_lowercase();
        let code = super::godot::strip_c_comments(&ctx.read(f));
        if name.contains("blur") {
            out.push(finding(L, f, 0, "blur shader; panels are opaque and docked (L1594)"));
        } else if blur_shader_code(&code) {
            out.push(finding(L, f, 0, "samples the screen texture with textureLod (a backdrop blur); panels are opaque and docked (L1594)"));
        }
    }
    for f in ctx.files("docs/", &["css", "html", "md", "scss"]).chain(ctx.files("README.md", &[])) {
        let text = ctx.read(f);
        for (i, l) in text.lines().enumerate() {
            if l.contains("backdrop-filter") { out.push(finding(L, f, (i + 1) as u32, "backdrop-filter is glassmorphism (L1594)")); }
        }
    }
    out
}

fn check_glass_scene(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "ui.glass";
    let mut out = Vec::new();
    if scrim(path) { return out; }
    let sections = parse_resource(text);
    for s in &sections {
        let ty = s.type_name().unwrap_or_default();
        let name = s.attr_str("name").unwrap_or_default();
        if s.kind == "node" && ty == "BackBufferCopy" && !scrim(&name) {
            out.push(finding(L, path, s.line, format!("BackBufferCopy {name}: no glass panels; panels are opaque and docked (L1594)")));
        }
        if s.kind == "sub_resource" && ty == "Shader" {
            if let Some(code) = s.prop("code") {
                if blur_shader_code(&unquote(&code.value)) { out.push(finding(L, path, s.line, "inline shader blurs the screen texture (L1594)")); }
            }
        }
        if s.kind == "ext_resource" {
            if let Some(p) = s.attr_str("path") {
                let lower = p.to_ascii_lowercase();
                if lower.ends_with(".gdshader") && lower.rsplit('/').next().unwrap_or("").contains("blur") {
                    out.push(finding(L, path, s.line, format!("uses the blur shader {p} (L1594)")));
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// radius_shadow
// ------------------------------------------------------------------------------------------------

const MAX_RADIUS_PX: f64 = 4.0;
const CORNERS: &[&str] = &["corner_radius_top_left", "corner_radius_top_right", "corner_radius_bottom_right", "corner_radius_bottom_left"];

fn style_type(s: &Section, file_type: Option<&str>) -> Option<String> {
    match s.kind.as_str() { "sub_resource" => s.type_name(), "resource" => file_type.map(str::to_string), _ => None }
}

/// Largest corner radius of a StyleBoxFlat section.
fn max_radius(s: &Section) -> f64 { CORNERS.iter().filter_map(|c| s.prop(c).and_then(|p| parse_num(&p.value))).fold(0.0, f64::max) }

fn casts_shadow(s: &Section) -> bool {
    s.prop("shadow_size").and_then(|p| parse_num(&p.value)).unwrap_or(0.0) > 0.0 && shadow_alpha(s) > 0.0
}
fn shadow_alpha(s: &Section) -> f64 {
    // Godot's StyleBoxFlat shadow_color default is Color(0, 0, 0, 0.6).
    s.prop("shadow_color").and_then(|p| ctor_args(&p.value, "Color")).and_then(|a| a.get(3).copied()).unwrap_or(0.6)
}

/// Saturated (non-grey) and visible colour: channels differ by more than half an 8-bit step and alpha > 0.
fn coloured(args: &[f64]) -> bool {
    if args.len() < 3 { return false; }
    let alpha = args.get(3).copied().unwrap_or(1.0);
    let max = args[..3].iter().cloned().fold(f64::MIN, f64::max);
    let min = args[..3].iter().cloned().fold(f64::MAX, f64::min);
    alpha > 0.0 && max - min > 0.5 / 255.0
}

/// Shadowed theme types: `Type/styles/panel` pointing at a shadow-casting StyleBoxFlat, with base types.
struct ThemeShadows { shadowed: Vec<String>, base: Vec<(String, String)> }
impl ThemeShadows {
    fn parse(text: &str) -> Self {
        let sections = parse_resource(text);
        let shadow_ids: Vec<String> = sections.iter().filter(|s| s.kind == "sub_resource" && s.type_name().as_deref() == Some("StyleBoxFlat") && casts_shadow(s)).filter_map(|s| s.attr_str("id")).collect();
        let mut shadowed = Vec::new();
        let mut base = Vec::new();
        for s in sections.iter().filter(|s| s.kind == "resource") {
            for p in &s.props {
                if let Some(ty) = p.key.strip_suffix("/styles/panel") {
                    if resource_refs(&p.value, "SubResource").iter().any(|id| shadow_ids.contains(id)) { shadowed.push(ty.to_string()); }
                }
                if let Some(ty) = p.key.strip_suffix("/base_type") { base.push((ty.to_string(), unquote(&p.value))); }
            }
        }
        ThemeShadows { shadowed, base }
    }
    fn is_shadowed(&self, ty: &str) -> bool {
        let mut t = ty.to_string();
        for _ in 0..8 {
            if self.shadowed.contains(&t) { return true; }
            match self.base.iter().find(|(v, _)| *v == t) { Some((_, b)) => t = b.clone(), None => return false }
        }
        false
    }
}

/// StyleBox corner radius > 4 px; two or more radii per screen; a shadowed PanelContainer inside another;
/// `shadow_color` with saturation above 0. Unit: `xt-lint-ui`.
pub fn radius_shadow(ctx: &LintCtx) -> Vec<Finding> {
    let theme = ThemeShadows::parse(&ctx.read(THEME));
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["tscn", "tres"]) { out.extend(check_radius_shadow(f, &ctx.read(f), &theme)); }
    out
}

fn check_radius_shadow(path: &str, text: &str, theme: &ThemeShadows) -> Vec<Finding> {
    const L: &str = "ui.radius-shadow";
    let sections = parse_resource(text);
    let file_type = sections.first().filter(|s| s.kind == "gd_resource").and_then(|s| s.type_name());
    let mut out = Vec::new();
    let mut radii: Vec<f64> = Vec::new();
    let mut shadow_boxes: Vec<String> = Vec::new();
    for s in &sections {
        for p in &s.props {
            if p.key.ends_with("shadow_color") {
                let drawn = style_type(s, file_type.as_deref()).as_deref() != Some("StyleBoxFlat") || s.prop("shadow_size").and_then(|q| parse_num(&q.value)).unwrap_or(0.0) > 0.0;
                if drawn && ctor_args(&p.value, "Color").map_or(false, |a| coloured(&a)) {
                    out.push(finding(L, path, p.line, format!("{} is not grey; shadows carry no hue (L1593)", p.key)));
                }
            }
        }
        if style_type(s, file_type.as_deref()).as_deref() != Some("StyleBoxFlat") { continue; }
        let r = max_radius(s);
        if r > MAX_RADIUS_PX { out.push(finding(L, path, s.line, format!("StyleBox corner radius {r} px; at most 4 (L1565)"))); }
        if r > 0.0 && !radii.contains(&r) { radii.push(r); }
        if casts_shadow(s) { if let Some(id) = s.attr_str("id") { shadow_boxes.push(id); } }
    }
    if path.ends_with(".tscn") {
        if radii.len() >= 2 {
            out.push(finding(L, path, 0, format!("{} different corner radii in one screen; one radius per screen (L1595)", radii.len())));
        }
        let nodes = scene_nodes(&sections);
        let shadowed = |n: &Node| -> bool {
            if n.ty != "PanelContainer" { return false; }
            if let Some(v) = n.prop_val("theme_override_styles/panel") { return resource_refs(v, "SubResource").iter().any(|id| shadow_boxes.contains(id)); }
            let var = variation(n);
            theme.is_shadowed(if var.is_empty() { "PanelContainer" } else { &var })
        };
        for n in nodes.iter().filter(|n| shadowed(*n)) {
            let mut p = parent_path(n);
            while let Some(pp) = p {
                let Some(anc) = nodes.iter().find(|a| a.path == pp) else { break };
                if shadowed(anc) {
                    out.push(finding(L, path, n.line, format!("shadowed PanelContainer {} sits inside shadowed {}; no nested cards (L1595)", n.path, anc.path)));
                    break;
                }
                p = parent_path(anc);
            }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// graphedit
// ------------------------------------------------------------------------------------------------

/// Properties that must be false (their defaults are true). `show_zoom_label` defaults to false and must stay so.
const GRAPHEDIT_OFF: &[&str] = &["show_menu", "show_zoom_buttons", "show_grid_buttons", "show_minimap_button", "show_arrange_button", "minimap_enabled"];

fn script_sets_false(script: &str, prop: &str) -> bool { gd_assignments(script, prop).iter().any(|(_, v)| v == "false") }

/// GraphEdit nodes must turn off menu, minimap, zoom, grid toggle, arrange and minimap buttons and zoom label.
/// Unit: `xt-lint-ui`.
pub fn graphedit(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["tscn", "gd"]) {
        let text = ctx.read(f);
        if f.ends_with(".gd") {
            out.extend(check_graphedit_script(f, &text));
            continue;
        }
        let sections = parse_resource(&text);
        let scripts: Vec<(String, String)> = sections.iter().filter(|s| s.kind == "ext_resource").filter_map(|s| {
            let p = s.attr_str("path")?;
            if !p.ends_with(".gd") { return None; }
            Some((s.attr_str("id")?, super::godot::res_to_repo(&p).map(|r| strip_gd_comments(&ctx.read(&r))).unwrap_or_default()))
        }).collect();
        out.extend(check_graphedit_scene(f, &sections, &scripts));
    }
    out
}

fn check_graphedit_scene(path: &str, sections: &[Section], scripts: &[(String, String)]) -> Vec<Finding> {
    const L: &str = "ui.graphedit";
    let mut out = Vec::new();
    for n in scene_nodes(sections).iter().filter(|n| n.ty == "GraphEdit") {
        let script = n.prop_val("script").and_then(|v| resource_refs(v, "ExtResource").into_iter().next()).and_then(|id| scripts.iter().find(|(i, _)| *i == id)).map(|(_, s)| s.as_str()).unwrap_or("");
        for prop in GRAPHEDIT_OFF {
            let off = match n.prop_val(prop) { Some(v) => parse_bool(v) == Some(false), None => script_sets_false(script, prop) };
            if !off { out.push(finding(L, path, n.prop(prop).map_or(n.line, |p| p.line), format!("GraphEdit {} must set {prop} = false (L1736)", n.path))); }
        }
        if n.prop_val("show_zoom_label").and_then(parse_bool) == Some(true) {
            out.push(finding(L, path, n.line, format!("GraphEdit {} shows the zoom label (L1736)", n.path)));
        }
    }
    out
}

fn check_graphedit_script(path: &str, text: &str) -> Vec<Finding> {
    let code = strip_gd_comments(text);
    let mut out = Vec::new();
    for at in find_ident(&code, "GraphEdit") {
        if !code[at + 9..].starts_with(".new") { continue; }
        for prop in GRAPHEDIT_OFF {
            if !script_sets_false(&code, prop) {
                out.push(finding("ui.graphedit", path, line_of(&code, at), format!("GraphEdit.new() without {prop} = false (L1736)")));
            }
        }
    }
    for (ln, v) in gd_assignments(&code, "show_zoom_label") {
        if v == "true" { out.push(finding("ui.graphedit", path, ln, "GraphEdit zoom label shown (L1736)")); }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// blink
// ------------------------------------------------------------------------------------------------

const BLINK_PROPS: &[&str] = &["visible", "modulate", "self_modulate"];
/// Allowed blink cycles: 1 Hz and 2.5 Hz.
const BLINK_CYCLES_S: &[f64] = &[1.0, 0.4];

fn allowed_cycle(s: f64) -> bool { BLINK_CYCLES_S.iter().any(|c| (c - s).abs() < 1e-3) }

/// Identifiers a script declares as labels (`var x: Label`, `... as Label`).
fn label_idents(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in code.lines() {
        let t = line.trim_start().trim_start_matches("@onready").trim_start().trim_start_matches("@export").trim_start();
        let Some(rest) = t.strip_prefix("var ") else { continue };
        let name: String = rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let after = &rest.trim_start()[name.len()..];
        let typed = after.trim_start().strip_prefix(':').map(|x| x.trim_start()).map_or(false, |x| x.starts_with("Label") || x.starts_with("RichTextLabel"));
        let cast = after.contains(" as Label") || after.contains(" as RichTextLabel");
        if !name.is_empty() && (typed || cast) { out.push(name); }
    }
    out
}

/// Looped tweens or AnimationPlayer tracks on a Label's `visible` or `modulate`; blink rates other than 1 Hz
/// and 2.5 Hz (cycle under 400 ms). Unit: `xt-lint-ui`.
pub fn blink(ctx: &LintCtx) -> Vec<Finding> {
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["gd", "tscn", "tres"]) {
        let text = ctx.read(f);
        if f.ends_with(".gd") { out.extend(check_blink_script(f, &text)); } else { out.extend(check_blink_scene(f, &text)); }
    }
    out
}

fn check_blink_script(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "ui.blink";
    let code = strip_gd_comments(text);
    let lines: Vec<&str> = code.lines().collect();
    let extends_label = lines.iter().any(|l| { let t = l.trim(); t == "extends Label" || t == "extends RichTextLabel" });
    let labels = label_idents(&code);
    let is_label = |target: &str| {
        let t = target.trim();
        if t == "self" { return extends_label; }
        let base: String = t.trim_start_matches('$').chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '/').collect();
        labels.iter().any(|l| *l == base) || base.to_ascii_lowercase().contains("label")
    };
    let mut out = Vec::new();
    for (s, e) in functions(&lines) {
        let body = &lines[s..e];
        let looped = body.iter().any(|l| l.contains("set_loops("));
        let mut blink_targets: Vec<(u32, String)> = Vec::new();
        let mut cycle = 0.0f64;
        let mut cycle_known = true;
        for (k, l) in body.iter().enumerate() {
            for (target, prop, _, dur) in tween_props(l) {
                match dur { Some(d) => cycle += d, None => cycle_known = false }
                if BLINK_PROPS.contains(&prop.as_str()) { blink_targets.push(((s + k + 1) as u32, target)); }
            }
            for at in find_ident(l, "tween_interval") {
                let rest = l[at + 14..].trim_start();
                match rest.strip_prefix('(').map(split_args).and_then(|a| a.first().and_then(|d| parse_num(d))) { Some(d) => cycle += d, None => cycle_known = false }
            }
        }
        if !looped || blink_targets.is_empty() { continue; }
        for (ln, target) in &blink_targets {
            if is_label(target) {
                out.push(finding(L, path, *ln, format!("looped tween on a Label's visibility or modulate ({target}); no blinking text, one symbol at most (L1603)")));
            }
        }
        if cycle_known && !allowed_cycle(cycle) {
            out.push(finding(L, path, (s + 1) as u32, format!("blink cycle {cycle} s; only 1 Hz (1 s) and 2.5 Hz (0.4 s) (L1603)")));
        }
    }
    // Toggling visibility by hand (`visible = not visible`) on a label.
    for (i, l) in lines.iter().enumerate() {
        for at in find_ident(l, "visible") {
            let rest = l[at + 7..].trim_start();
            let Some(v) = rest.strip_prefix('=') else { continue };
            let v = v.trim();
            if !(v.starts_with("not ") || v.starts_with('!')) || !v.contains("visible") { continue; }
            let target = l[..at].trim().trim_end_matches('.');
            let target = target.rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
            if is_label(if target.is_empty() { "self" } else { target }) {
                out.push(finding(L, path, (i + 1) as u32, "toggles a Label's visibility; no blinking text (L1603)"));
            }
        }
    }
    out
}

/// Join a node path relative to `base` (both in [`Node::path`] form), resolving `..`.
fn join_node_path(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base == "." { Vec::new() } else { base.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    if parts.is_empty() { ".".to_string() } else { parts.join("/") }
}

fn check_blink_scene(path: &str, text: &str) -> Vec<Finding> {
    const L: &str = "ui.blink";
    let sections = parse_resource(text);
    let nodes = scene_nodes(&sections);
    let by_id = |id: &str| sections.iter().find(|s| s.kind == "sub_resource" && s.attr_str("id").as_deref() == Some(id));
    // Animation id -> base node path its tracks resolve against.
    let mut owner: Vec<(String, String)> = Vec::new();
    for n in nodes.iter().filter(|n| n.ty == "AnimationPlayer") {
        let root = n.prop_val("root_node").map(|v| unquote(v.trim().strip_prefix("NodePath(").and_then(|x| x.strip_suffix(')')).unwrap_or(v))).unwrap_or_else(|| "..".into());
        let base = join_node_path(&n.path, &root);
        let libs: Vec<String> = n.props.iter().filter(|p| p.key == "libraries" || p.key.starts_with("libraries/")).flat_map(|p| resource_refs(&p.value, "SubResource")).collect();
        for lib in libs {
            if let Some(ls) = by_id(&lib) {
                for p in ls.props.iter().filter(|p| p.key == "_data") {
                    for anim in resource_refs(&p.value, "SubResource") { owner.push((anim, base.clone())); }
                }
            }
        }
    }
    let mut out = Vec::new();
    for s in sections.iter().filter(|s| is_animation(s, &sections)) {
        let looped = s.prop("loop_mode").and_then(|p| parse_num(&p.value)).unwrap_or(0.0) != 0.0;
        if !looped { continue; }
        let id = s.attr_str("id").unwrap_or_default();
        let base = owner.iter().find(|(a, _)| *a == id).map(|(_, b)| b.clone());
        let mut blinks = false;
        for (ln, target, prop) in animation_tracks(s) {
            if !BLINK_PROPS.contains(&prop.as_str()) { continue; }
            blinks = true;
            let node = base.as_ref().and_then(|b| nodes.iter().find(|n| n.path == join_node_path(b, &target)));
            let label = node.map_or_else(|| target.to_ascii_lowercase().contains("label"), |n| is_label_type(&n.ty));
            if label { out.push(finding(L, path, ln, format!("looped animation on a Label's {prop} ({target}); no blinking text (L1603)"))); }
        }
        if blinks {
            let len = s.prop("length").and_then(|p| parse_num(&p.value)).unwrap_or(1.0);
            if !allowed_cycle(len) { out.push(finding(L, path, s.line, format!("looped blink of {len} s; only 1 Hz and 2.5 Hz (L1603)"))); }
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// tooltips
// ------------------------------------------------------------------------------------------------

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect()
}

/// Whether a tooltip restates its label: every label word is in it and it adds fewer than 4 words (L1609).
fn restates(label: &str, tooltip: &str) -> bool {
    let l = words(label);
    let mut t = words(tooltip);
    if l.is_empty() || t.is_empty() { return false; }
    for w in &l {
        match t.iter().position(|x| x == w) { Some(i) => { t.remove(i); } None => return false }
    }
    t.len() < 4
}

/// A tooltip whose text is the control's label plus fewer than 4 words (icon-only controls exempt).
/// Unit: `xt-lint-ui`.
pub fn tooltips(ctx: &LintCtx) -> Vec<Finding> {
    let english = super::copy::english_map(ctx);
    let mut out = Vec::new();
    for f in ui_scope(ctx, &["tscn"]) { out.extend(check_tooltips(f, &ctx.read(f), &english)); }
    out
}

fn check_tooltips(path: &str, text: &str, english: &[(String, String)]) -> Vec<Finding> {
    let resolve = |v: &str| {
        let s = unquote(v);
        english.binary_search_by(|(k, _)| k.as_str().cmp(&s)).map(|i| english[i].1.clone()).unwrap_or(s)
    };
    let mut out = Vec::new();
    for n in scene_nodes(&parse_resource(text)) {
        let (Some(label), Some(tip)) = (n.prop_val("text"), n.prop("tooltip_text")) else { continue };
        let (label, tooltip) = (resolve(label), resolve(&tip.value));
        if restates(&label, &tooltip) {
            out.push(finding("ui.tooltip", path, tip.line, format!("tooltip \"{tooltip}\" restates the label \"{label}\"; give the effect with units (L1609)")));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(body: &str) -> String { format!("[gd_scene format=3]\n\n[node name=\"Root\" type=\"Control\"]\n{body}") }

    #[test]
    fn overrides_in_scenes_and_scripts() {
        let t = scene("theme_override_colors/font_color = Color(1, 0, 0, 1)\ntheme_override_constants/separation = 4\n");
        assert_eq!(check_theme_overrides("game/ui/x.tscn", &t).len(), 1);
        let gd = "func _ready():\n\tadd_theme_color_override(\"font_color\", c)\n\tadd_theme_constant_override(\"separation\", 4)\n\tset(\"theme_override_fonts/font\", f)\n\t# add_theme_font_override( in a comment\n";
        assert_eq!(check_theme_overrides("game/ui/x.gd", gd).len(), 2);
    }

    #[test]
    fn colour_literals_and_scope() {
        let gd = "var a := Color(1, 0, 0)\nvar b := Color.RED\nvar c := Color.html(\"#fff\")\nvar d: Color\nvar e := Color8(1, 2, 3)\nvar f := \"#a0b0c0\"\nvar g := PackedColorArray()\n";
        assert_eq!(check_colour_literals("game/ui/x.gd", gd).iter().map(|f| f.line).collect::<Vec<_>>(), vec![1, 2, 3, 5, 6]);
        assert!(colour_exempt("game/ui/theme/theme_compiler.gd"));
        assert!(colour_exempt("game/world/world.tscn"));
        assert!(colour_exempt("game/test/lint/ui_lint.gd"));
        assert!(!colour_exempt("game/ui/hud/hud.tscn"));
        assert!(!colour_exempt("game/boot/boot.gd"));
    }

    #[test]
    fn tween_rules() {
        let gd = "func flash() -> void:\n\tvar t := create_tween()\n\tt.set_trans(Tween.TRANS_BOUNCE).set_ease(Tween.EASE_IN_OUT)\n\tt.tween_property(panel, \"position:x\", 10.0, 0.15)\n\nfunc ok() -> void:\n\tvar t := create_tween()\n\tt.set_meta(\"sim_signal\", \"alert_raised\")\n\tt.set_ease(Tween.EASE_OUT)\n\tt.tween_property(box, \"offset_transform_position\", Vector2(4, 0), 0.15)\n";
        let f = check_tweens("game/ui/x.gd", gd);
        // bounce, ease in-out, position, unnamed tween.
        assert_eq!(f.len(), 4, "{f:#?}");
        let t = scene("[node name=\"Anim\" type=\"AnimationPlayer\" parent=\".\"]\n[node name=\"Anim2\" type=\"AnimationPlayer\" parent=\".\"]\nmetadata/sim_signal = \"radar_emitting\"\n");
        assert_eq!(check_tweens("game/ui/x.tscn", &t).len(), 1);
        let anim = "[gd_scene format=3]\n[sub_resource type=\"Animation\" id=\"a\"]\ntracks/0/type = \"value\"\ntracks/0/path = NodePath(\"Box:size\")\n[node name=\"Root\" type=\"Control\"]\n";
        assert_eq!(check_tweens("game/ui/y.tscn", anim).len(), 1);
    }

    #[test]
    fn uppercase_anywhere() {
        assert_eq!(check_uppercase("game/ui/x.tscn", &scene("[node name=\"L\" type=\"Label\" parent=\".\"]\nuppercase = true\n")).len(), 1);
        assert_eq!(check_uppercase("game/ui/x.gd", "func f():\n\tlabel.uppercase = true\n\tother.uppercase = false\n").len(), 1);
    }

    #[test]
    fn eyebrow_rules() {
        let t = scene(concat!(
            "[node name=\"Col\" type=\"VBoxContainer\" parent=\".\"]\n",
            "[node name=\"Small\" type=\"Label\" parent=\"Col\"]\n",
            "[node name=\"Title\" type=\"Label\" parent=\"Col\"]\ntheme_type_variation = &\"PanelTitle\"\n",
            "[node name=\"Hull\" type=\"Label\" parent=\"Col\"]\ntheme_type_variation = &\"Meta\"\n",
            "[node name=\"Date\" type=\"Label\" parent=\"Col\"]\ntheme_type_variation = &\"Meta\"\n",
            "[node name=\"KickerText\" type=\"Label\" parent=\".\"]\n",
            "[node name=\"Row\" type=\"HBoxContainer\" parent=\".\"]\n",
            "[node name=\"Side\" type=\"Label\" parent=\"Row\"]\n",
            "[node name=\"Title2\" type=\"Label\" parent=\"Row\"]\ntheme_type_variation = &\"ScreenTitle\"\n",
        ));
        let keys = vec!["ui/x.tscn#Col/Hull".to_string()];
        let f = check_eyebrow_scene("game/ui/x.tscn", &t, &keys);
        // label above title, unbound Date, two Metas for zero panels (1 allowed), Kicker name.
        assert_eq!(f.len(), 4, "{f:#?}");
        let pill = scene(concat!(
            "[node name=\"Col\" type=\"VBoxContainer\" parent=\".\"]\n",
            "[node name=\"Pill\" type=\"PanelContainer\" parent=\"Col\"]\n",
            "[node name=\"Tag\" type=\"Label\" parent=\"Col/Pill\"]\n",
            "[node name=\"Title\" type=\"Label\" parent=\"Col\"]\ntheme_type_variation = &\"MenuTitle\"\n",
        ));
        assert_eq!(check_eyebrow_scene("game/ui/p.tscn", &pill, &[]).len(), 1);
        assert_eq!(read_meta_fields("# c\nres://ui/x.tscn#Col/Hull = vessel.hull_number\n")[0].0, "ui/x.tscn#Col/Hull");
        assert_eq!(check_eyebrow_script("game/ui/x.gd", "class_name SectionLabelBox\n").len(), 1);
    }

    #[test]
    fn glass_rules() {
        let t = scene("[node name=\"Back\" type=\"BackBufferCopy\" parent=\".\"]\n");
        assert_eq!(check_glass_scene("game/ui/x.tscn", &t).len(), 1);
        assert!(check_glass_scene("game/ui/pause_scrim.tscn", &t).is_empty());
        assert!(blur_shader_code("shader_type canvas_item;\nuniform sampler2D s : hint_screen_texture;\nvoid fragment(){ COLOR = textureLod(s, SCREEN_UV, 2.0); }\n"));
        assert!(!blur_shader_code("shader_type canvas_item;\nvoid fragment(){ COLOR = vec4(1.0); }\n"));
    }

    #[test]
    fn radius_and_shadow_rules() {
        let theme_text = "[gd_resource type=\"Theme\" format=3]\n[sub_resource type=\"StyleBoxFlat\" id=\"card\"]\nshadow_size = 4\nshadow_color = Color(0, 0, 0, 0.3)\n[resource]\nCard/base_type = &\"PanelContainer\"\nCard/styles/panel = SubResource(\"card\")\n";
        let theme = ThemeShadows::parse(theme_text);
        assert!(theme.is_shadowed("Card"));
        assert!(!theme.is_shadowed("PanelContainer"));
        assert!(check_radius_shadow("game/ui/theme/project_theme.tres", theme_text, &theme).is_empty());
        let t = format!("{}{}", "[gd_scene format=3]\n[sub_resource type=\"StyleBoxFlat\" id=\"a\"]\ncorner_radius_top_left = 6\n[sub_resource type=\"StyleBoxFlat\" id=\"b\"]\ncorner_radius_top_left = 2\nshadow_size = 2\nshadow_color = Color(0.2, 0.1, 0.4, 0.5)\n",
            "[node name=\"Root\" type=\"Control\"]\n[node name=\"Outer\" type=\"PanelContainer\" parent=\".\"]\ntheme_type_variation = &\"Card\"\n[node name=\"Inner\" type=\"PanelContainer\" parent=\"Outer\"]\ntheme_type_variation = &\"Card\"\n");
        let f = check_radius_shadow("game/ui/x.tscn", &t, &theme);
        // radius 6, coloured shadow, two radii, nested shadowed panels.
        assert_eq!(f.len(), 4, "{f:#?}");
        assert!(!coloured(&[0.08, 0.1, 0.12, 0.0]));
        assert!(!coloured(&[0.2, 0.2, 0.2, 1.0]));
        assert!(coloured(&[0.2, 0.2, 0.3, 1.0]));
    }

    #[test]
    fn graphedit_chrome() {
        let s = parse_resource(&scene("[node name=\"G\" type=\"GraphEdit\" parent=\".\"]\nshow_menu = false\nshow_zoom_buttons = false\nshow_grid_buttons = false\nshow_minimap_button = false\nshow_arrange_button = false\n"));
        assert_eq!(check_graphedit_scene("game/ui/g.tscn", &s, &[]).len(), 1);
        let s2 = parse_resource("[gd_scene format=3]\n[ext_resource type=\"Script\" path=\"res://ui/g.gd\" id=\"1\"]\n[node name=\"G\" type=\"GraphEdit\"]\nscript = ExtResource(\"1\")\n");
        let script = "func _ready():\n\tshow_menu = false\n\tshow_zoom_buttons = false\n\tshow_grid_buttons = false\n\tshow_minimap_button = false\n\tshow_arrange_button = false\n\tminimap_enabled = false\n".to_string();
        assert!(check_graphedit_scene("game/ui/g.tscn", &s2, &[("1".into(), script)]).is_empty());
        assert_eq!(check_graphedit_script("game/ui/x.gd", "var g := GraphEdit.new()\n").len(), 6);
    }

    #[test]
    fn blink_rules() {
        let gd = "extends Control\n@onready var status_label: Label = $Status\n@onready var dot: TextureRect = $Dot\n\nfunc blink_label() -> void:\n\tvar t := create_tween().set_loops()\n\tt.tween_property(status_label, \"modulate:a\", 0.0, 0.2)\n\tt.tween_property(status_label, \"modulate:a\", 1.0, 0.2)\n\nfunc blink_dot() -> void:\n\tvar t := create_tween().set_loops()\n\tt.tween_property(dot, \"modulate:a\", 0.0, 0.1)\n\tt.tween_property(dot, \"modulate:a\", 1.0, 0.1)\n\nfunc ok_dot() -> void:\n\tvar t := create_tween().set_loops()\n\tt.tween_property(dot, \"modulate:a\", 0.0, 0.2)\n\tt.tween_property(dot, \"modulate:a\", 1.0, 0.2)\n";
        let f = check_blink_script("game/ui/x.gd", gd);
        // two label tweens, one bad dot cycle (0.2 s).
        assert_eq!(f.len(), 3, "{f:#?}");
        assert_eq!(check_blink_script("game/ui/l.gd", "extends Label\nfunc _on_timer():\n\tvisible = not visible\n").len(), 1);
        let t = concat!(
            "[gd_scene format=3]\n",
            "[sub_resource type=\"Animation\" id=\"blink\"]\nlength = 0.3\nloop_mode = 1\ntracks/0/path = NodePath(\"Text:visible\")\n",
            "[sub_resource type=\"AnimationLibrary\" id=\"lib\"]\n_data = {\n&\"blink\": SubResource(\"blink\")\n}\n",
            "[node name=\"Root\" type=\"Control\"]\n[node name=\"Text\" type=\"Label\" parent=\".\"]\n",
            "[node name=\"Anim\" type=\"AnimationPlayer\" parent=\".\"]\nlibraries = {\n&\"\": SubResource(\"lib\")\n}\n",
        );
        assert_eq!(check_blink_scene("game/ui/b.tscn", t).len(), 2);
        assert_eq!(join_node_path("A/Anim", ".."), "A");
        assert_eq!(join_node_path("Anim", ".."), ".");
        assert_eq!(join_node_path(".", "Text"), "Text");
    }

    #[test]
    fn tooltip_restating() {
        assert!(restates("Fire control", "Fire control: controls firing"));
        assert!(restates("Radar", "Radar"));
        assert!(!restates("Radar", "Radar: shows contacts within 40 km of own ship"));
        assert!(!restates("", "Rotate 90 degrees (R)"));
        let english = vec![("ui.fc".to_string(), "Fire control".to_string()), ("ui.fc.tip".to_string(), "Fire control: controls firing".to_string())];
        let t = scene("[node name=\"B\" type=\"Button\" parent=\".\"]\ntext = \"ui.fc\"\ntooltip_text = \"ui.fc.tip\"\n[node name=\"I\" type=\"Button\" parent=\".\"]\ntooltip_text = \"Radar (R)\"\n");
        assert_eq!(check_tooltips("game/ui/t.tscn", &t, &english).len(), 1);
    }
}
