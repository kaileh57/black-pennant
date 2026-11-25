extends SceneTree
## Godot-side UI lints, headless (DESIGN.md L1749-1755: CI on UI changes runs every lint). From game/:
##   godot --headless --path . -s res://test/lint/ui_lint.gd              self-test, then lint res://
##   godot --headless --path . -s res://test/lint/ui_lint.gd -- --self-test   self-test only
## Prints `path:line: [lint] message` per finding and exits 1 on any finding or self-test failure.
## What runs here needs the engine: scene trees, the theme against the default theme, font
## coverage. Text greps over the repository are `cargo xtask lint`. Unit: ui-lint.

const Rules := preload("res://test/lint/lint_rules.gd")
const Compiler := preload("res://ui/theme/theme_compiler.gd")
const SKIP_DIRS: Array[String] = ["res://.godot", "res://addons", "res://test/lint"]
const TEXT_SCENE_DIRS: Array[String] = ["res://ui/", "res://boot/"]
const FONT_EXTENSIONS: Array[String] = ["ttf", "otf", "woff", "woff2", "fnt", "font", "pfb", "pfm"]

var _self_failures := 0


func _initialize() -> void:
	var only_self := OS.get_cmdline_user_args().has("--self-test")
	_self_test()
	if _self_failures > 0:
		printerr("ui_lint: self-test FAILED (%d)" % _self_failures)
		quit(1)
		return
	print("ui_lint: self-test ok")
	if only_self:
		quit(0)
		return
	var rules := Rules.new()
	_lint_project(rules)
	for f: Dictionary in rules.findings:
		printerr("%s:%d: [%s] %s" % [String(f["path"]).trim_prefix("res://"), int(f["line"]), f["lint"], f["message"]])
	print("ui_lint: %d findings" % rules.findings.size())
	quit(1 if not rules.findings.is_empty() else 0)


# --- project -------------------------------------------------------------------------------------

func _collect(dir_path: String, out: Array[String]) -> void:
	for skip: String in SKIP_DIRS:
		if dir_path == skip:
			return
	var dir := DirAccess.open(dir_path)
	if dir == null:
		return
	for sub: String in dir.get_directories():
		if sub.begins_with("."):
			continue
		_collect(dir_path.path_join(sub), out)
	for file: String in dir.get_files():
		out.append(dir_path.path_join(file))


func _lint_project(rules: Rules) -> void:
	var files: Array[String] = []
	_collect("res://", files)
	files.sort()
	# Theme: compiled, current, complete against the default theme, with every variation.
	var compiler := Compiler.new()
	for path: String in compiler.stale_outputs():
		rules.add("theme.stale", path, 0, "out of date with res://ui/theme/tokens.json; run res://ui/theme/compile_theme.gd")
	for message: String in compiler.errors:
		rules.add("theme.compile", Compiler.TOKENS_PATH, 0, message)
	var theme_types: PackedStringArray = []
	var names: PackedStringArray = PackedStringArray(Compiler.LABEL_VARIATIONS)
	for family: String in Compiler.STRIPE_STATES:
		for state: String in Compiler.STRIPE_STATES[family]:
			names.append(family + state)
	for path: String in Compiler.OUTPUTS.values():
		var theme := load(path) as Theme
		if theme == null:
			rules.add("theme.compile", path, 0, "cannot load")
			continue
		rules.check_theme_diff(path, theme, ThemeDB.get_default_theme())
		rules.check_theme_variations(path, theme, names)
		rules.check_theme_styles(path, theme)
		for t: String in theme.get_type_list():
			if not theme_types.has(t):
				theme_types.append(t)
	rules.check_tokens(Compiler.TOKENS_PATH, rules.tokens)
	# Scenes and scripts.
	for path: String in files:
		var ext := path.get_extension()
		if ext == "tscn":
			var scene := ResourceLoader.load(path, "PackedScene") as PackedScene
			if scene == null:
				rules.add("scene.load", path, 0, "cannot load")
				continue
			var state := scene.get_state()
			for dir: String in TEXT_SCENE_DIRS:
				if path.begins_with(dir):
					rules.check_scene_text(path, state)
			rules.check_scene_looks(path, state, theme_types)
		elif ext == "gd":
			rules.check_script(path, FileAccess.get_file_as_string(path))
	# Copy and fonts.
	var chains := _font_chains(rules)
	for path: String in files:
		if path.begins_with("res://i18n/") and path.get_extension() == "csv":
			var strings := Rules.csv_strings(FileAccess.get_file_as_string(path))
			rules.check_copy(path, strings)
			rules.check_coverage(path, strings, chains)
	_check_font_files(rules, files)


## Font chain per locale: Atkinson first (digits stay Atkinson), then the locale's OFL fallback.
func _font_chains(rules: Rules) -> Dictionary:
	var fonts: Dictionary = rules.tokens.get("fonts", {})
	var sans_path: String = fonts.get("sans", {}).get("file", "")
	var sans := load(sans_path) as Font if ResourceLoader.exists(sans_path) else null
	var chains := {}
	if sans == null:
		rules.add("font.files", Compiler.TOKENS_PATH, 0, "sans font %s missing" % sans_path)
		return chains
	chains["en"] = [sans]
	var fallbacks: Dictionary = fonts.get("fallbacks", {})
	for locale: String in fallbacks:
		var chain: Array = [sans]
		var fb_path: String = fallbacks[locale].get("file", "") if fallbacks[locale] is Dictionary else ""
		if ResourceLoader.exists(fb_path):
			chain.append(load(fb_path))
		chains[locale] = chain
		chains[locale.split("_")[0]] = chain
	return chains


## L1599: CI fails on font files not listed in the tokens; listed fonts ship their licence.
func _check_font_files(rules: Rules, files: Array[String]) -> void:
	var listed: Dictionary = {}
	var fonts: Dictionary = rules.tokens.get("fonts", {})
	for key: String in fonts:
		var entries: Array = []
		if key == "fallbacks":
			for locale: String in fonts[key]:
				entries.append(fonts[key][locale])
		else:
			entries.append(fonts[key])
		for entry: Variant in entries:
			if not entry is Dictionary:
				continue
			for field: String in ["file", "msdf_file"]:
				if entry.has(field):
					listed[entry[field]] = entry.get("licence", "")
	for path: String in files:
		if path.get_extension().to_lower() in FONT_EXTENSIONS or path.ends_with(".msdf.res"):
			if not listed.has(path):
				rules.add("font.files", path, 0, "font file not listed in res://ui/theme/tokens.json (L1599)")
			elif path.get_extension() != "res" and not FileAccess.file_exists(String(listed[path])):
				rules.add("font.files", path, 0, "licence %s missing" % listed[path])


# --- self-test -----------------------------------------------------------------------------------

func _expect(rules: Rules, lint: String, n_min: int, what: String) -> void:
	var n := rules.count(lint)
	if (n_min == 0 and n != 0) or (n_min > 0 and n < n_min):
		_self_failures += 1
		printerr("ui_lint self-test: %s: expected %s %s, got %d" % [what, "none" if n_min == 0 else "at least %d" % n_min, lint, n])


func _pack(root: Node) -> SceneState:
	for child in root.find_children("*", "", true, false):
		child.owner = root
	var scene := PackedScene.new()
	scene.pack(root)
	root.free()
	return scene.get_state()


func _label(name: String, variation: String) -> Label:
	var l := Label.new()
	l.name = name
	l.theme_type_variation = variation
	return l


func _self_test() -> void:
	var probe := Rules.new()
	var title_var: String = probe.pat("title_variation")[0]
	var meta_var: String = probe.pat("meta_variation")[0]
	var bad_name: String = probe.pat("eyebrow_name")[0]

	# Eyebrow: a label, or a pill holding one, directly above a title in a column.
	var r := Rules.new()
	var col := VBoxContainer.new()
	col.name = "Screen"
	col.add_child(_label("Above", "Body"))
	col.add_child(_label("Title", title_var))
	r.check_scene_text("res://ui/fixture_a.tscn", _pack(col))
	_expect(r, "ui.eyebrow", 1, "label above title")
	r = Rules.new()
	col = VBoxContainer.new()
	col.name = "Screen"
	var pill := PanelContainer.new()
	pill.name = "Badge"
	pill.add_child(_label("Word", "Dense"))
	col.add_child(pill)
	col.add_child(_label("Title", title_var))
	r.check_scene_text("res://ui/fixture_b.tscn", _pack(col))
	_expect(r, "ui.eyebrow", 1, "pill above title")
	r = Rules.new()
	var row := HBoxContainer.new()
	row.name = "Header"
	row.add_child(_label("Title", title_var))
	row.add_child(_label("Number", "Body"))
	var col2 := VBoxContainer.new()
	col2.name = "Screen"
	col2.add_child(row)
	col2.add_child(_label("Body", "Body"))
	r.check_scene_text("res://ui/fixture_c.tscn", _pack(col2))
	_expect(r, "ui.eyebrow", 0, "label beside a title")
	# Names, uppercase, Meta binding and count.
	r = Rules.new()
	var panel := PanelContainer.new()
	panel.name = "Panel"
	var inner := VBoxContainer.new()
	inner.name = "Column"
	panel.add_child(inner)
	inner.add_child(_label("Title", title_var))
	inner.add_child(_label("Hull", meta_var))
	inner.add_child(_label("Date", meta_var))
	var shout := _label("My" + bad_name, "Body")
	shout.set(probe.pat("case_property")[0], true)
	inner.add_child(shout)
	r.meta_fields = {"ui/fixture_d.tscn#Column/Hull": "Hud.own_ship().hull_number"}
	r.check_scene_text("res://ui/fixture_d.tscn", _pack(panel))
	_expect(r, "ui.eyebrow-name", 1, "eyebrow node name")
	_expect(r, "ui.uppercase", 1, "uppercase label")
	_expect(r, "ui.meta-unbound", 1, "unbound Meta label")
	_expect(r, "ui.meta-excess", 1, "two Meta lines on one panel")
	# Looks: overrides, colour literals, unknown variations, local themes.
	r = Rules.new()
	var styled := _label("Styled", "NoSuchVariation")
	styled.set(probe.pat("override_property")[0] + "font_color", Compiler.hex("#" + "ff00ff"))
	styled.self_modulate = Compiler.hex("#" + "808080")
	styled.theme = Theme.new()
	r.check_scene_looks("res://ui/fixture_e.tscn", _pack(styled), PackedStringArray(["Body"]))
	_expect(r, "ui.theme-override", 1, "theme colour override")
	_expect(r, "ui.colour-literal", 1, "modulate literal")
	_expect(r, "ui.unknown-variation", 1, "unknown variation")
	_expect(r, "ui.local-theme", 1, "local theme")
	r = Rules.new()
	r.check_scene_looks("res://ui/fixture_f.tscn", _pack(_label("Plain", "Body")), PackedStringArray(["Body"]))
	_expect(r, "ui.theme-override", 0, "clean label")
	_expect(r, "ui.colour-literal", 0, "clean label")
	# Scripts: transitions, eases, layout tweens, durations, sim-signal metadata, overrides, colours.
	r = Rules.new()
	# Built in pieces so this file carries none of the patterns it tests.
	var col_word := "Col" + "or"
	var lines: PackedStringArray = [
		"func a() -> void:",
		"\tvar t := create_tween()",
		"\tt.set_trans(Tween.%s)" % probe.pat("banned_transition")[0],
		"\tt.tween_property(self, \"%s\", Vector2.ZERO, 0.3)" % probe.pat("layout_property")[0],
		"\tt.set_ease(Tween.%s)" % probe.pat("banned_ease")[0],
		"\t%s&\"font_color\", x)" % probe.pat("override_call")[0],
		"\tvar c := %s8(1, 2, 3)" % col_word,
		"",
		"func b() -> void:",
		"\tvar t := create_tween()",
		"\tt.set_meta(&\"%s\", &\"alert_raised\")" % probe.pat("sim_signal_meta")[0],
		"\tt.tween_property(self, \"offset_transform_position\", Vector2.ZERO, 0.15)",
		"\t# %s(1, 1, 1) in a comment is fine" % col_word,
	]
	r.check_script("res://ui/fixture.gd", "\n".join(lines))
	_expect(r, "ui.tween-transition", 1, "banned transition")
	_expect(r, "ui.tween-layout", 1, "layout tween")
	_expect(r, "ui.tween-ease", 1, "ease in")
	_expect(r, "ui.tween-duration", 1, "0.3 s duration")
	_expect(r, "ui.tween-signal", 1, "tween without sim signal")
	_expect(r, "ui.theme-override", 1, "override call")
	_expect(r, "ui.colour-literal", 1, "Color8 literal")
	if r.count("ui.tween-signal") > 1 or r.count("ui.tween-duration") > 1 or r.count("ui.colour-literal") > 1:
		_self_failures += 1
		printerr("ui_lint self-test: the clean function b() was flagged")
	r = Rules.new()
	r.check_script("res://ui/theme/fixture.gd", "\n".join(lines))
	if r.count("ui.colour-literal") != 0:
		_self_failures += 1
		printerr("ui_lint self-test: res://ui/theme/ must be exempt from colour literals")
	# Copy: emoji and banned words on word boundaries, English only.
	r = Rules.new()
	var word: String = r.banned[0]
	var csv := "keys,?context,en,de\n"
	csv += "a.one,note,\"Fire %s\",x\n" % String.chr(0x1F525)
	csv += "a.two,note,\"%s the plan\",y\n" % word.capitalize()
	csv += "a.three,note,\"Re%sing plans\",z\n" % word
	csv += "a.four,note,\"Range 12 %sm\",w\n" % String.chr(0xB5)
	var strings := Rules.csv_strings(csv)
	r.check_copy("res://i18n/fixture.csv", strings)
	_expect(r, "copy.emoji", 1, "emoji")
	if r.count("copy.banned") != 1:
		_self_failures += 1
		printerr("ui_lint self-test: banned words: expected exactly 1 (word boundaries), got %d" % r.count("copy.banned"))
	var sans := load(r.tokens.get("fonts", {}).get("sans", {}).get("file", "")) as Font
	if sans != null:
		r.check_coverage("res://i18n/fixture.csv", strings, {"en": [sans], "de": [sans]})
		# The emoji row and the µ row; the banned-word rows are plain Latin.
		if r.count("font.coverage") != 2:
			_self_failures += 1
			printerr("ui_lint self-test: coverage: expected the emoji and µ rows, got %d" % r.count("font.coverage"))
	# Theme: an empty theme misses every item; tokens with a wrong size and ratio are caught.
	r = Rules.new()
	var empty := Theme.new()
	r.check_theme_diff("res://fixture.tres", empty, ThemeDB.get_default_theme())
	_expect(r, "theme.default-diff", 100, "empty theme against the default theme")
	r = Rules.new()
	var round_theme := Theme.new()
	var round_box := StyleBoxFlat.new()
	round_box.set_corner_radius_all(8)
	round_box.shadow_size = 4
	round_theme.set_stylebox("panel", "PanelContainer", round_box)
	r.check_theme_styles("res://fixture.tres", round_theme)
	_expect(r, "theme.radius", 1, "radius 8")
	_expect(r, "theme.shadow", 1, "shadowed panel")
	r = Rules.new()
	r.check_tokens("res://fixture.json", r.tokens)
	_expect(r, "tokens.type-scale", 0, "committed tokens")
	_expect(r, "tokens.contrast", 0, "committed tokens")
	var broken: Dictionary = r.tokens.duplicate(true)
	broken["type"]["Body"]["font_size"] = 21
	broken["contrast"]["dusk"]["text.primary"] = 12.0
	broken["radius"]["tile"] = 6
	r = Rules.new()
	r.check_tokens("res://fixture.json", broken)
	_expect(r, "tokens.type-scale", 1, "wrong font size")
	_expect(r, "tokens.contrast", 1, "wrong ratio")
	_expect(r, "tokens.radius", 1, "radius over 4")
