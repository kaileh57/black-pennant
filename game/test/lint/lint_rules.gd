extends RefCounted
## Godot-side UI lint rules. DESIGN.md L1568-1618 (eyebrow and banned patterns), L1620-1651
## (tokens), L1728-1742 (implementation), L1749-1755 (review). Unit: ui-lint.
##
## Each rule takes plain inputs (a SceneState, script text, CSV rows, a Theme) and appends
## findings, so ui_lint.gd runs them over res:// and the self-test runs them on fixtures built
## in memory. Patterns and word lists live in res://ui/lint/*.txt.

const PATTERNS_PATH := "res://ui/lint/godot_patterns.txt"
const META_FIELDS_PATH := "res://ui/lint/meta_fields.txt"
const BANNED_PATH := "res://ui/lint/banned_phrases.txt"
const TOKENS_PATH := "res://ui/theme/tokens.json"
const ICON_DIR := "res://ui/icons/"
const Compiler := preload("res://ui/theme/theme_compiler.gd")

var findings: Array[Dictionary] = []
var patterns: Dictionary[String, PackedStringArray] = {}
var meta_fields: Dictionary[String, String] = {}
var banned: PackedStringArray = []
var tokens: Dictionary = {}

var _colour_regexes: Array[RegEx] = []
var _banned_regexes: Array[RegEx] = []
var _emoji_ranges: Array[Vector2i] = []


func _init() -> void:
	patterns = read_patterns(FileAccess.get_file_as_string(PATTERNS_PATH))
	meta_fields = read_meta_fields(FileAccess.get_file_as_string(META_FIELDS_PATH))
	banned = read_list(FileAccess.get_file_as_string(BANNED_PATH))
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(TOKENS_PATH))
	tokens = parsed if parsed is Dictionary else {}
	compile_patterns()


# --- lists ---------------------------------------------------------------------------------------

static func read_list(text: String) -> PackedStringArray:
	var out: PackedStringArray = []
	for raw: String in text.split("\n"):
		var line := raw.strip_edges()
		if line.is_empty() or line.begins_with("#"):
			continue
		out.append(line)
	return out


static func read_patterns(text: String) -> Dictionary[String, PackedStringArray]:
	var out: Dictionary[String, PackedStringArray] = {}
	for line: String in read_list(text):
		var colon := line.find(":")
		if colon <= 0:
			continue
		var key := line.substr(0, colon).strip_edges()
		var value := line.substr(colon + 1).strip_edges()
		if not out.has(key):
			out[key] = PackedStringArray()
		out[key].append(value)
	return out


## meta_fields.txt: `<scene path>#<node path> = <source>`; scene paths may be res:// or game/.
static func read_meta_fields(text: String) -> Dictionary[String, String]:
	var out: Dictionary[String, String] = {}
	for line: String in read_list(text):
		var eq := line.find("=")
		if eq <= 0:
			continue
		var key := normalise_scene_key(line.substr(0, eq).strip_edges())
		out[key] = line.substr(eq + 1).strip_edges()
	return out


static func normalise_scene_key(key: String) -> String:
	var k := key.trim_prefix("res://").trim_prefix("game/")
	return k


func compile_patterns() -> void:
	_colour_regexes.clear()
	for source: String in patterns.get("colour_regex", PackedStringArray()):
		var re := RegEx.create_from_string(source)
		if re != null and re.is_valid():
			_colour_regexes.append(re)
	_banned_regexes.clear()
	for phrase: String in banned:
		var escaped := ""
		for ch in phrase.to_lower():
			escaped += ("\\" + ch) if "\\^$.|?*+()[]{}-".contains(ch) else ch
		_banned_regexes.append(RegEx.create_from_string("(?i)(?<![\\p{L}\\p{N}])" + escaped + "(?![\\p{L}\\p{N}])"))
	_emoji_ranges.clear()
	for r: String in patterns.get("emoji_range", PackedStringArray()):
		var parts := r.split("-")
		if parts.size() == 2:
			_emoji_ranges.append(Vector2i(parts[0].hex_to_int(), parts[1].hex_to_int()))


func pat(key: String) -> PackedStringArray:
	return patterns.get(key, PackedStringArray())


func add(lint: String, path: String, line: int, message: String) -> void:
	findings.append({"lint": lint, "path": path, "line": line, "message": message})


func count(lint: String) -> int:
	var n := 0
	for f: Dictionary in findings:
		if f["lint"] == lint:
			n += 1
	return n


func colour_exempt(path: String) -> bool:
	for dir: String in pat("colour_exempt_dir"):
		if path.begins_with(dir):
			return true
	return false


# --- scenes --------------------------------------------------------------------------------------

## Flattened view of a SceneState: one Dictionary per node with name, parent, type (instanced
## scenes resolve to their root type), properties and child order.
static func scene_nodes(state: SceneState) -> Array[Dictionary]:
	var nodes: Array[Dictionary] = []
	for i in state.get_node_count():
		var props: Dictionary = {}
		for p in state.get_node_property_count(i):
			props[String(state.get_node_property_name(i, p))] = state.get_node_property_value(i, p)
		var type := String(state.get_node_type(i))
		var instance := state.get_node_instance(i)
		if type.is_empty() and instance != null:
			var sub := instance.get_state()
			if sub.get_node_count() > 0:
				type = String(sub.get_node_type(0))
				# Properties set on the instance root inside its own scene still apply.
				for p in sub.get_node_property_count(0):
					var pname := String(sub.get_node_property_name(0, p))
					if not props.has(pname):
						props[pname] = sub.get_node_property_value(0, p)
		var path := String(state.get_node_path(i))
		var parent := String(state.get_node_path(i, true)) if i > 0 else ""
		nodes.append({"index": i, "name": String(state.get_node_name(i)), "path": path, "parent": parent, "type": type, "props": props, "instance": instance})
	return nodes


static func is_a(type: String, base: String) -> bool:
	if type.is_empty():
		return false
	if type == base:
		return true
	return ClassDB.class_exists(type) and ClassDB.is_parent_class(type, base)


static func is_text_label(node: Dictionary) -> bool:
	return is_a(node["type"], "Label") or is_a(node["type"], "RichTextLabel")


static func variation_of(node: Dictionary) -> String:
	return String(node["props"].get("theme_type_variation", ""))


func is_title(node: Dictionary) -> bool:
	return is_text_label(node) and variation_of(node) in pat("title_variation")


static func children_of(nodes: Array[Dictionary], parent_path: String) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for n: Dictionary in nodes:
		if n["index"] > 0 and n["parent"] == parent_path:
			out.append(n)
	return out


static func node_by_path(nodes: Array[Dictionary], path: String) -> Dictionary:
	for n: Dictionary in nodes:
		if n["path"] == path:
			return n
	return {}


## Leaves under a node that are text labels, and whether anything else is there (a pill is a
## panel or container that holds only a short label).
func _only_labels(nodes: Array[Dictionary], node: Dictionary) -> bool:
	if is_text_label(node):
		return not is_title(node)
	if not is_a(node["type"], "Container"):
		return false
	var kids := children_of(nodes, node["path"])
	if kids.is_empty() or kids.size() > 2:
		return false
	for kid: Dictionary in kids:
		if not _only_labels(nodes, kid):
			return false
	return true


func _stacked_vertically(parent: Dictionary) -> bool:
	var t: String = parent.get("type", "")
	if is_a(t, "VBoxContainer") or is_a(t, "VFlowContainer"):
		return true
	if is_a(t, "BoxContainer") and bool(parent["props"].get("vertical", false)):
		return true
	return false


## L1578: no label (or pill holding one) directly before a title; no eyebrow names; Meta lines
## bound to meta_fields.txt and at most ceil(panels / 4) per screen; Label.uppercase false.
func check_scene_text(path: String, state: SceneState) -> void:
	var nodes := scene_nodes(state)
	var scene_key := normalise_scene_key(path)
	var banned_names := pat("eyebrow_name")
	var panels := 0
	var metas: Array[Dictionary] = []
	for n: Dictionary in nodes:
		for bad: String in banned_names:
			if String(n["name"]).to_lower().contains(bad.to_lower()):
				add("ui.eyebrow-name", path, 0, "node %s is named like an eyebrow (%s)" % [n["path"], bad])
		if is_a(n["type"], "Panel") or is_a(n["type"], "PanelContainer"):
			panels += 1
		for prop: String in pat("case_property"):
			if bool(n["props"].get(prop, false)):
				add("ui.uppercase", path, 0, "%s sets %s; use sentence case" % [n["path"], prop])
		if is_text_label(n) and variation_of(n) in pat("meta_variation"):
			metas.append(n)
		if is_title(n):
			_check_title(path, nodes, n)
	for m: Dictionary in metas:
		var key := "%s#%s" % [scene_key, String(m["path"]).trim_prefix("./")]
		if not meta_fields.has(key):
			add("ui.meta-unbound", path, 0, "Meta label %s is not bound in res://ui/lint/meta_fields.txt (%s)" % [m["path"], key])
	var allowed := ceili(maxi(panels, 1) / 4.0)
	if metas.size() > allowed:
		add("ui.meta-excess", path, 0, "%d Meta lines for %d panels; at most %d" % [metas.size(), panels, allowed])


func _check_title(path: String, nodes: Array[Dictionary], title: Dictionary) -> void:
	var parent := node_by_path(nodes, title["parent"])
	if parent.is_empty() or not _stacked_vertically(parent):
		return
	var siblings := children_of(nodes, title["parent"])
	var at := -1
	for i in siblings.size():
		if siblings[i]["path"] == title["path"]:
			at = i
	if at <= 0:
		return
	var before: Dictionary = siblings[at - 1]
	if _only_labels(nodes, before):
		add("ui.eyebrow", path, 0, "%s sits directly above the title %s; put facts in the heading or a bound Meta line after it" % [before["path"], title["path"]])


## L1728: no colour, font, font size or style overrides; no local Theme; no colour literals;
## known type variations; textures from the icon set only.
func check_scene_looks(path: String, state: SceneState, theme_types: PackedStringArray) -> void:
	var nodes := scene_nodes(state)
	var exempt := colour_exempt(path)
	for n: Dictionary in nodes:
		var props: Dictionary = n["props"]
		for prop: String in props:
			for prefix: String in pat("override_property"):
				if prop.begins_with(prefix):
					add("ui.theme-override", path, 0, "%s sets %s; use a type variation" % [n["path"], prop])
			var value: Variant = props[prop]
			if prop == "theme" and value is Theme and not String((value as Theme).resource_path).begins_with("res://ui/theme/"):
				add("ui.local-theme", path, 0, "%s carries its own Theme; looks come from the project theme" % n["path"])
			if not exempt:
				_check_value_colour(path, n["path"], prop, value)
			if value is Texture2D and is_a(n["type"], "Control"):
				_check_texture(path, n["path"], prop, value)
		var variation := variation_of(n)
		if not variation.is_empty() and not theme_types.has(variation):
			add("ui.unknown-variation", path, 0, "%s uses theme_type_variation %s, which the project theme lacks" % [n["path"], variation])
		if is_a(n["type"], "AnimationPlayer"):
			_check_animation_player(path, n)


func _check_value_colour(path: String, node_path: String, prop: String, value: Variant) -> void:
	if value is Color:
		add("ui.colour-literal", path, 0, "%s.%s holds a colour literal; read it from the theme" % [node_path, prop])
	elif value is Resource and String((value as Resource).resource_path).is_empty() and (value is StyleBox or value is Font or value is Theme):
		add("ui.colour-literal", path, 0, "%s.%s embeds a %s; looks come from the project theme" % [node_path, prop, (value as Resource).get_class()])


func _check_texture(path: String, node_path: String, prop: String, tex: Texture2D) -> void:
	var res_path := String(tex.resource_path)
	if res_path.is_empty() or res_path.contains("::"):
		return
	for dir: String in pat("texture_dir"):
		if res_path.begins_with(dir):
			return
	add("ui.icon-source", path, 0, "%s.%s uses %s; icons come only from %s" % [node_path, prop, res_path, ICON_DIR])


## L1737: AnimationPlayers name their sim signal and never animate layout.
func _check_animation_player(path: String, n: Dictionary) -> void:
	var meta_key := "metadata/" + (pat("sim_signal_meta")[0] if not pat("sim_signal_meta").is_empty() else "sim_signal")
	if not n["props"].has(meta_key):
		add("ui.tween-signal", path, 0, "AnimationPlayer %s has no %s metadata naming its sim signal" % [n["path"], meta_key])
	var libraries: Variant = n["props"].get("libraries", {})
	if libraries is Dictionary:
		for lib_name: Variant in libraries:
			var lib := libraries[lib_name] as AnimationLibrary
			if lib == null:
				continue
			for anim_name: StringName in lib.get_animation_list():
				var anim := lib.get_animation(anim_name)
				for t in anim.get_track_count():
					var track_path := String(anim.track_get_path(t))
					var prop := track_path.get_slice(":", 1) if track_path.contains(":") else ""
					if prop in pat("layout_property"):
						add("ui.tween-layout", path, 0, "animation %s animates %s; animate offset_transform_* only" % [anim_name, track_path])


# --- scripts -------------------------------------------------------------------------------------

static func strip_comment(line: String) -> String:
	var in_str := ""
	for i in line.length():
		var ch := line[i]
		if in_str.is_empty():
			if ch == "#":
				return line.substr(0, i)
			if ch == "\"" or ch == "'":
				in_str = ch
		elif ch == in_str and (i == 0 or line[i - 1] != "\\"):
			in_str = ""
	return line


## L1593, L1728, L1737 over one script's text.
func check_script(path: String, text: String) -> void:
	var lines := text.split("\n")
	var exempt := colour_exempt(path)
	var durations: Array[float] = []
	var motion: Dictionary = tokens.get("motion_ms", {})
	for key: String in motion:
		durations.append(float(motion[key]) / 1000.0)
	var dur_re := RegEx.create_from_string("tween_property\\s*\\((.*),\\s*([0-9]*\\.?[0-9]+)\\s*\\)")
	var func_re := RegEx.create_from_string("^\\s*(static\\s+)?func\\s")
	var func_start := 0
	var func_has_tween := false
	var func_has_meta := false
	var meta_key: String = pat("sim_signal_meta")[0] if not pat("sim_signal_meta").is_empty() else "sim_signal"
	for i in lines.size() + 1:
		var at_end := i == lines.size()
		var code := "" if at_end else strip_comment(lines[i])
		if at_end or func_re.search(code) != null:
			if func_has_tween and not func_has_meta:
				add("ui.tween-signal", path, func_start + 1, "a Tween is created without set_meta(\"%s\", ...) naming its sim signal" % meta_key)
			func_start = i
			func_has_tween = false
			func_has_meta = false
		if at_end:
			break
		var ln := i + 1
		for call: String in pat("override_call"):
			if code.contains(call):
				add("ui.theme-override", path, ln, "%s; use a type variation" % call.trim_suffix("("))
		if not exempt:
			for re: RegEx in _colour_regexes:
				if re.search(code) != null:
					add("ui.colour-literal", path, ln, "colour literal; read colours from the theme or tokens")
					break
		for trans: String in pat("banned_transition"):
			if RegEx.create_from_string("\\b" + trans + "\\b").search(code) != null:
				add("ui.tween-transition", path, ln, trans + " is banned; motion only reports state")
		for ease: String in pat("banned_ease"):
			if RegEx.create_from_string("\\b" + ease + "\\b").search(code) != null:
				add("ui.tween-ease", path, ln, ease + "; non-linear tweens use EASE_OUT")
		if code.contains("tween_property("):
			for prop: String in pat("layout_property"):
				if RegEx.create_from_string("[\"'&^]" + prop + "[\"':]").search(code) != null:
					add("ui.tween-layout", path, ln, "tweens %s; animate offset_transform_* (visual only)" % prop)
			var m := dur_re.search(code)
			if m != null:
				var d := float(m.get_string(2))
				var ok := false
				for allowed: float in durations:
					if absf(allowed - d) < 0.0005:
						ok = true
				if not ok:
					add("ui.tween-duration", path, ln, "duration %s s is not a motion token" % m.get_string(2))
		if code.contains("create_tween(") or code.contains("Tween.new("):
			func_has_tween = true
		if code.contains("set_meta(") and code.contains(meta_key):
			func_has_meta = true


# --- copy ----------------------------------------------------------------------------------------

func is_emoji(cp: int) -> bool:
	for r: Vector2i in _emoji_ranges:
		if cp >= r.x and cp <= r.y:
			return true
	return false


## Rows of a CSV in the I18n format (keys,?context,<locales>), as {key, locale, text, line}.
static func csv_strings(text: String) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	var tmp := FileAccess.create_temp(FileAccess.WRITE_READ, "lint", "csv")
	if tmp == null:
		return out
	tmp.store_string(text)
	tmp.seek(0)
	var header := tmp.get_csv_line()
	var line := 1
	while not tmp.eof_reached():
		var row := tmp.get_csv_line()
		line += 1
		if row.size() < 3 or row[0].is_empty():
			continue
		for c in range(2, mini(row.size(), header.size())):
			var locale := header[c]
			if locale.begins_with("?") or locale.begins_with("_") or locale == "keys" or row[c].is_empty():
				continue
			out.append({"key": row[0], "locale": locale, "text": row[c], "line": line})
	tmp.close()
	return out


## L1606, L1702-1703: no emoji; no banned words or phrases in English copy (word boundaries).
func check_copy(path: String, strings: Array[Dictionary]) -> void:
	for s: Dictionary in strings:
		var text: String = s["text"]
		for i in text.length():
			if is_emoji(text.unicode_at(i)):
				add("copy.emoji", path, s["line"], "%s (%s) contains emoji U+%X" % [s["key"], s["locale"], text.unicode_at(i)])
				break
		if s["locale"] != "en":
			continue
		for k in _banned_regexes.size():
			if _banned_regexes[k].search(text) != null:
				add("copy.banned", path, s["line"], "%s uses \"%s\" (res://ui/lint/banned_phrases.txt)" % [s["key"], banned[k]])


## L1645: every character of a locale's strings is in that locale's font chain.
func check_coverage(path: String, strings: Array[Dictionary], chains: Dictionary) -> void:
	for s: Dictionary in strings:
		var chain: Array = chains.get(s["locale"], chains.get(String(s["locale"]).split("_")[0], chains.get("en", [])))
		var text: String = s["text"]
		var missing: PackedStringArray = []
		for i in text.length():
			var cp := text.unicode_at(i)
			if cp < 0x20 or cp == 0x7F:
				continue
			var found := false
			for font: Font in chain:
				if font.has_char(cp):
					found = true
					break
			if not found and not missing.has("U+%04X" % cp):
				missing.append("U+%04X" % cp)
		if not missing.is_empty():
			add("font.coverage", path, s["line"], "%s (%s): %s missing from the font chain" % [s["key"], s["locale"], ", ".join(missing)])


# --- theme ---------------------------------------------------------------------------------------

## L1742: every item of every default-theme type exists in the project theme, and none of them
## is the default look (Godot's textures or fonts).
func check_theme_diff(path: String, theme: Theme, base: Theme) -> void:
	var data_types := {Theme.DATA_TYPE_COLOR: "colour", Theme.DATA_TYPE_CONSTANT: "constant", Theme.DATA_TYPE_FONT: "font", Theme.DATA_TYPE_FONT_SIZE: "font size", Theme.DATA_TYPE_ICON: "icon", Theme.DATA_TYPE_STYLEBOX: "style"}
	for type: String in base.get_type_list():
		for dt: int in data_types:
			for item: String in base.get_theme_item_list(dt, type):
				if not theme.has_theme_item(dt, item, type):
					add("theme.default-diff", path, 0, "%s %s/%s is missing; the default look would show" % [data_types[dt], type, item])
					continue
				var value: Variant = theme.get_theme_item(dt, item, type)
				if dt == Theme.DATA_TYPE_ICON:
					_check_theme_icon(path, type, item, value)
				elif dt == Theme.DATA_TYPE_FONT and value == base.get_theme_item(dt, item, type):
					add("theme.default-diff", path, 0, "font %s/%s is the engine default" % [type, item])
	if theme.default_font == null:
		add("theme.default-diff", path, 0, "default_font unset; the engine font would show")


func _check_theme_icon(path: String, type: String, item: String, tex: Variant) -> void:
	if tex is DPITexture:
		var stem := String((tex as DPITexture).resource_name)
		if not FileAccess.file_exists(ICON_DIR + stem + ".svg"):
			add("theme.icon-source", path, 0, "icon %s/%s (%s) is not from %s" % [type, item, stem, ICON_DIR])
		return
	if tex is ImageTexture and (tex as ImageTexture).get_image() == null:
		return
	if tex is GradientTexture2D and type == "ColorPicker":
		return
	add("theme.icon-source", path, 0, "icon %s/%s is not from %s" % [type, item, ICON_DIR])


## L1565, L1595: no style box in the theme rounds past radius.max or casts a shadow.
func check_theme_styles(path: String, theme: Theme) -> void:
	var max_r := int(tokens.get("radius", {}).get("max", 4))
	for type: String in theme.get_type_list():
		for item: String in theme.get_stylebox_list(type):
			var sb := theme.get_stylebox(item, type) as StyleBoxFlat
			if sb == null:
				continue
			var r := maxi(maxi(sb.corner_radius_top_left, sb.corner_radius_top_right), maxi(sb.corner_radius_bottom_left, sb.corner_radius_bottom_right))
			if r > max_r:
				add("theme.radius", path, 0, "%s/%s has radius %d; at most %d" % [type, item, r, max_r])
			if sb.shadow_size > 0:
				add("theme.shadow", path, 0, "%s/%s casts a shadow" % [type, item])


## L1729: the named type variations exist as registered variations, none named like a base
## type (an engine class name cannot be a variation at all; see VARIATION_ALIASES).
func check_theme_variations(path: String, theme: Theme, names: PackedStringArray) -> void:
	var types := theme.get_type_list()
	for name: String in names:
		if not types.has(name):
			add("theme.variations", path, 0, "type variation %s is missing" % name)
		elif theme.get_type_variation_base(name) == &"":
			add("theme.variations", path, 0, "%s is not registered as a variation, so Controls ignore it" % name)
		if ClassDB.class_exists(name):
			add("theme.variations", path, 0, "%s is an engine class name" % name)


# --- tokens --------------------------------------------------------------------------------------

static func luminance(c: Color) -> float:
	var lin := c.srgb_to_linear()
	return 0.2126 * lin.r + 0.7152 * lin.g + 0.0722 * lin.b


static func contrast(a: Color, b: Color) -> float:
	var la := luminance(a)
	var lb := luminance(b)
	return (maxf(la, lb) + 0.05) / (minf(la, lb) + 0.05)


static func truncate2(v: float) -> float:
	return floorf(v * 100.0 + 0.000001) / 100.0


## L1624-1639, L1647, L1714: the type scale formula, the spec's contrast table and the floors.
func check_tokens(path: String, t: Dictionary) -> void:
	var fonts: Dictionary = t.get("fonts", {})
	var types: Dictionary = t.get("type", {})
	for name: String in types:
		var entry: Dictionary = types[name]
		var family: String = entry.get("font", "sans")
		var ratio: float = fonts.get(family, {}).get("height_ratio", 0.91)
		var want := ceili(float(entry.get("height_px", 0)) / ratio - 0.0001)
		if int(entry.get("font_size", 0)) != want:
			add("tokens.type-scale", path, 0, "%s font_size %d, but ceil(%s / %s) is %d" % [name, int(entry.get("font_size", 0)), str(entry.get("height_px")), str(ratio), want])
		if float(entry.get("height_px", 0)) < 18.0:
			add("tokens.type-scale", path, 0, "%s body height under the 18 px floor" % name)
	var table: Dictionary = t.get("contrast", {})
	var palettes: Dictionary = t.get("palettes", {})
	for pal_name: String in ["dusk", "high_contrast"]:
		var pal: Dictionary = palettes.get(pal_name, {})
		var spec: Dictionary = table.get(pal_name, {})
		if pal.is_empty():
			add("tokens.contrast", path, 0, "palette %s missing" % pal_name)
			continue
		var on := Compiler.hex(pal.get(table.get("on", "surface.raised"), ""))
		for token: String in spec:
			var got := truncate2(contrast(Compiler.hex(pal.get(token, "")), on))
			if absf(got - float(spec[token])) > 0.001:
				add("tokens.contrast", path, 0, "%s %s on surface.raised is %.2f; the spec table says %.2f" % [pal_name, token, got, float(spec[token])])
		var floor_text := 7.0 if pal_name == "high_contrast" else 4.5
		for token: String in ["text.primary", "text.secondary"]:
			if contrast(Compiler.hex(pal.get(token, "")), on) < floor_text:
				add("tokens.contrast", path, 0, "%s %s under %.1f:1" % [pal_name, token, floor_text])
		for token: String in ["text.inactive", "ui.focus", "select", "stance.friend", "stance.hostile", "stance.neutral", "stance.suspect", "alert.caution"]:
			if contrast(Compiler.hex(pal.get(token, "")), on) < 3.0:
				add("tokens.contrast", path, 0, "%s %s under 3:1 for frames and icons" % [pal_name, token])
	for pair: Variant in table.get("pairs", []):
		var p: Array = pair
		var pal2: Dictionary = palettes.get(p[0], {})
		var got2 := truncate2(contrast(Compiler.hex(pal2.get(p[1], "")), Compiler.hex(pal2.get(p[2], ""))))
		if absf(got2 - float(p[3])) > 0.001:
			add("tokens.contrast", path, 0, "%s %s on %s is %.2f; the spec says %.2f" % [p[0], p[1], p[2], got2, float(p[3])])
	var radius: Dictionary = t.get("radius", {})
	for key: String in radius:
		if int(radius[key]) > 4:
			add("tokens.radius", path, 0, "radius %s is %d; lint fails above 4 (L1565)" % [key, int(radius[key])])
	var blink: Dictionary = t.get("blink_hz", {})
	for key: String in blink:
		if not [1.0, 2.5].has(float(blink[key])):
			add("tokens.blink", path, 0, "blink %s at %s Hz; only 1 and 2.5 Hz (L1603)" % [key, str(blink[key])])
