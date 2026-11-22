extends SceneTree
## Headless checks for the menu, settings, campaign and setup screens.
## Units: ui-menu, ui-settings, ui-campaign-menu, ui-skirmish. Run from game/:
##   godot --headless --path . -s res://test/unit/ui_menus_test.gd
## Exits 1 on any failure. The real Settings and Editor are swapped for in-memory fakes while the
## test runs, so nothing is written to user://settings.cfg and no unfinished facade is called;
## Content (landed) is read for real.

const SCREENS := "res://ui/screens/"
const MY_FILES: Array[String] = [
	"main_menu", "pause_menu", "credits", "settings", "settings_bindings", "campaign_slots",
	"campaign_new", "skirmish_setup", "sandbox_setup", "menu_kit", "conditions_panel", "design_picker",
]
## First key segments this test resolves: the screens' own CSVs and the shared ones they reuse.
const KEY_ROOTS: Array[String] = [
	"menu", "pause", "credits", "settings_ui", "bindings", "campaign_menu", "skirmish", "sandbox",
	"settings", "units", "notes", "difficulty", "boot", "shell", "content",
]


## Stands in for the Settings autoload: a schema and a binding table in memory.
class FakeSettings extends Node:
	signal changed(key: StringName)
	signal bindings_changed
	var rows: Array[Dictionary] = []
	var values: Dictionary = {}
	var binding_rows: Array[Dictionary] = []
	var rebinds: Array[Dictionary] = []
	var unbinds: Array = []
	var resets: Array = []
	var conflict_once := false

	func schema() -> Array[Dictionary]:
		var out: Array[Dictionary] = []
		for r: Dictionary in rows:
			var d := r.duplicate()
			d["value"] = values.get(String(r["key"]), r.get("default"))
			out.append(d)
		return out

	func get_value(key: StringName) -> Variant:
		for r: Dictionary in rows:
			if r["key"] == key:
				return values.get(String(key), r.get("default"))
		return null

	func set_value(key: StringName, value: Variant) -> bool:
		values[String(key)] = value
		changed.emit(key)
		return true

	func reset_section(section: StringName) -> void:
		resets.append(section)
		for r: Dictionary in rows:
			if r["section"] == section:
				values.erase(String(r["key"]))
				changed.emit(r["key"])

	func bindings() -> Array[Dictionary]:
		return binding_rows

	func event_text(event: InputEvent) -> String:
		if event is InputEventKey:
			return OS.get_keycode_string((event as InputEventKey).physical_keycode)
		if event is InputEventJoypadButton:
			return "Pad %d" % (event as InputEventJoypadButton).button_index
		return "Mouse"

	func rebind(action: StringName, device: StringName, index: int, event: InputEvent, replace: bool) -> Dictionary:
		rebinds.append({"action": action, "device": device, "index": index, "event": event, "replace": replace})
		if conflict_once and not replace:
			conflict_once = false
			return {"ok": false, "conflict": &"other", "text": {"key": &"settings.rebind.conflict", "args": ["W", {"key": &"settings.action.fire"}]}}
		# The real Settings emits before it returns.
		bindings_changed.emit()
		return {"ok": true, "conflict": &"", "text": {"key": &"settings.rebind.bound", "args": ["W", {"key": &"settings.action.fire"}]}}

	func unbind(action: StringName, device: StringName, index: int) -> bool:
		unbinds.append([action, device, index])
		bindings_changed.emit()
		return true

	func reset_bindings(device: StringName) -> void:
		resets.append(device)
		bindings_changed.emit()


## Stands in for the Editor autoload for the design picker.
class FakeEditor extends Node:
	var issues: Dictionary = {}

	func file_issues(path: String, _ruleset: String) -> Array:
		return issues.get(path, [])

	func browse(_folder: String) -> Array:
		return []

	func is_dirty() -> bool:
		return false

	func in_splash() -> bool:
		return false


var _failures := 0
var _checks := 0
var _frame := 0
var _settings: FakeSettings
var _editor: FakeEditor
var _real: Dictionary[String, Node] = {}
var _parked: Array[Array] = []


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 2:
		_swap_in_fakes()
		_key_checks()
		_kit_checks()
		_settings_checks()
		_main_menu_checks()
		_pause_checks()
		_credits_checks()
		_campaign_checks()
		_setup_checks()
	elif _frame == 4:
		_finish()
	elif _frame > 600:
		printerr("FAIL: ui_menus_test did not finish (a script error stopped it)")
		quit(1)
	return false


func _finish() -> void:
	await _bindings_checks()
	await _measure_parked()
	await _settings_pages_fit()
	_swap_out_fakes()
	print("ui_menus_test: %d checks, %d failures" % [_checks, _failures])
	quit(1 if _failures > 0 else 0)


func _ok(cond: bool, what: String) -> void:
	_checks += 1
	if not cond:
		_failures += 1
		printerr("FAIL: ", what)


func _swap_in_fakes() -> void:
	_settings = FakeSettings.new()
	_editor = FakeEditor.new()
	for pair: Array in [["Settings", _settings], ["Editor", _editor]]:
		var real := root.get_node_or_null(NodePath(pair[0]))
		if real != null:
			real.name = pair[0] + "Real"
			_real[pair[0]] = real
		(pair[1] as Node).name = pair[0]
		root.add_child(pair[1])


func _swap_out_fakes() -> void:
	for n: String in _real:
		var fake := root.get_node_or_null(NodePath(n))
		if fake != null:
			root.remove_child(fake)
			fake.free()
		_real[n].name = n


func _instance(stem: String) -> Node:
	var node := (load(SCREENS + stem + ".tscn") as PackedScene).instantiate()
	root.add_child(node)
	return node


# --- every key the screens name resolves -----------------------------------------------------------

func _key_checks() -> void:
	var re := RegEx.create_from_string("\"([a-z][a-z0-9_]*(?:\\.[a-z0-9_]+)+)\"")
	var missing := PackedStringArray()
	var seen := 0
	for stem: String in MY_FILES:
		for ext: String in ["gd", "tscn"]:
			var path := SCREENS + stem + "." + ext
			if not FileAccess.file_exists(path):
				continue
			for m: RegExMatch in re.search_all(FileAccess.get_file_as_string(path)):
				var key := m.get_string(1)
				if not KEY_ROOTS.has(key.get_slice(".", 0)) or key.ends_with(".gd") or key.ends_with(".tscn"):
					continue
				seen += 1
				if String(TranslationServer.translate(StringName(key))) == key:
					missing.append("%s (%s.%s)" % [key, stem, ext])
	_ok(seen > 150, "the key scan found the screens' keys (%d)" % seen)
	_ok(missing.is_empty(), "every key resolves: missing " + ", ".join(missing))
	# Keys built at run time.
	var built: Array[String] = []
	for c in 9:
		built.append("skirmish.sea.%d" % c)
	for w: String in ["clear", "haze", "overcast", "rain", "storm", "fog_bank"]:
		built.append("units.word.weather." + w)
	for w: String in ["night", "dusk", "day"]:
		built.append("units.word.time." + w)
	for k: String in ["ai_accuracy", "ai_reaction_s", "adaptation_cadence", "hull_count", "economy", "clock_days"]:
		built.append("campaign_menu.slider." + k)
		built.append("campaign_menu.slider." + k + ".help")
	for s: String in ["access", "hud", "input", "editor", "graphics", "audio", "net", "privacy", "playtest"]:
		built.append("settings.section." + s)
	for tag: String in ["crane", "mine_rail", "rhib_davit", "boarding_kit", "recovery_net", "camera_mast", "passenger_berths",
			"point_defence", "magazine", "fuel_store", "propulsion", "steering", "generation", "low_signature"]:
		built.append("notes.tag_word." + tag)
	for a in 4:
		built.append("units.word.act.%d" % (a + 1))
	for key: String in built:
		_ok(String(TranslationServer.translate(StringName(key))) != key, "built key resolves: " + key)


func _kit_checks() -> void:
	var Kit: GDScript = load(SCREENS + "menu_kit.gd")
	_ok(Kit.decimals_for(0.05) == 2 and Kit.decimals_for(0.1) == 1 and Kit.decimals_for(10.0) == 0, "decimals for slider steps")
	_ok(Kit.percent(0.7) == "70%", "a fraction reads as a percent: " + Kit.percent(0.7))
	_ok(Kit.percent(1.25) == "125%", "a multiplier reads as a percent: " + Kit.percent(1.25))
	var problems: PackedStringArray = Kit.problem_texts(null, &"menu.load.failed_body")
	_ok(problems.size() == 1 and problems[0] == tr(&"menu.load.failed_body"), "a failed facade call is one problem")
	_ok(Kit.problem_texts([], &"menu.load.failed_body").is_empty(), "an empty result is no problem")
	var issue: PackedStringArray = Kit.problem_texts([{"text": {"key": &"menu.load.failed_body"}, "fatal": true}, 7], &"x")
	_ok(issue.size() == 1, "IssueDicts give their sentence; junk entries are dropped")
	_ok(Kit.get_int({"a": 2.7}, "a") == 2 and Kit.get_int({"a": "x"}, "a", 5) == 5, "safe int reads")
	_ok(Kit.get_float({"a": NAN}, "a", 1.0) == 1.0, "a NaN reads as the fallback")
	_ok(Kit.local_time(0) == "", "no time for unix 0")
	_ok(Kit.local_time(1763100000).length() == 16, "local time is YYYY-MM-DD HH:MM")
	var inline: Button = Kit.inline_button("sandbox.targets.add")
	root.add_child(inline)
	_ok(inline.get_combined_minimum_size().x > Kit.BUTTON_H, "an inline button is as wide as its text (%s)" % str(inline.get_combined_minimum_size()))
	inline.free()
	var presets: Array[Dictionary] = [{"preset": "hard"}, {"preset": "custom_x"}, {"preset": "normal"}, {"preset": "easy"}]
	var ordered: Array = Kit.difficulty_order(presets).map(func(p: Dictionary) -> String: return p["preset"])
	_ok(ordered == ["easy", "normal", "hard", "custom_x"], "difficulty reads easiest first, unknown last")
	var Conditions: GDScript = load(SCREENS + "conditions_panel.gd")
	_ok(Conditions.seed_value("") == 0 and Conditions.seed_value(" 42 ") == 42, "blank and numeric seeds")
	_ok(Conditions.seed_value("harbour") == Conditions.seed_value("harbour") and Conditions.seed_value("harbour") > 0, "word seeds hash the same each time")


# --- settings ------------------------------------------------------------------------------------

func _fake_schema() -> void:
	var real := _real.get("Settings") as Node
	var rows: Array[Dictionary] = []
	if real != null:
		for d: Variant in real.call(&"schema"):
			rows.append((d as Dictionary).duplicate())
	_settings.rows = rows


func _settings_checks() -> void:
	_fake_schema()
	_ok(_settings.rows.size() > 30, "the real schema has its rows")
	var screen := _instance("settings")
	screen.call(&"setup", {"section": &"access"})
	var sections: Array = screen.get(&"_sections")
	var expected: Array[StringName] = []
	var access_rows := 0
	for r: Dictionary in _settings.rows:
		if bool(r.get("visible", false)):
			if not expected.has(StringName(r["section"])):
				expected.append(StringName(r["section"]))
			if r["section"] == &"access":
				access_rows += 1
	_ok(sections == expected, "one page per section with a visible row: %s" % str(sections))
	var rows: Dictionary = screen.get(&"_rows")
	_ok(rows.size() == access_rows, "the access page has its %d visible rows (%d)" % [access_rows, rows.size()])
	_ok(not rows.has(&"access.first_launch_done"), "internal flags are not shown")
	var motion: CheckButton = rows[&"access.reduce_motion"]["control"]
	motion.button_pressed = true
	_ok(_settings.values.get("access.reduce_motion") == true, "a toggle commits at once")
	var scale_entry: Dictionary = rows[&"access.ui_scale"]
	var slider: HSlider = scale_entry["control"]
	_ok((scale_entry["value"] as Label).text == "100%", "UI scale reads 100%")
	screen.get(&"_dragging")[&"access.ui_scale"] = true
	slider.value = 1.5
	_ok(not _settings.values.has("access.ui_scale"), "UI scale waits while the slider is dragged")
	_ok((scale_entry["value"] as Label).text == "150%", "the value follows the drag")
	screen.call(&"_on_drag_ended", true, &"access.ui_scale", slider)
	_ok(is_equal_approx(float(_settings.values.get("access.ui_scale", 0.0)), 1.5), "UI scale commits when the drag ends")
	# An outside change shows in the row without committing again.
	_settings.set_value(&"access.reduce_motion", false)
	_ok(not motion.button_pressed, "an outside change shows in its row")
	_ok(screen.get(&"_preview_panel") != null, "the access page has the officer line preview")
	_settings.set_value(&"access.officer_line_opacity", 0.25)
	_ok(is_equal_approx((screen.get(&"_preview_panel") as Control).self_modulate.a, 0.25), "the preview backing follows the opacity")
	# Privacy: -1 (not asked) selects no option; the page shows what a report holds.
	screen.call(&"_show_section", &"privacy")
	var privacy: OptionButton = (screen.get(&"_rows") as Dictionary)[&"privacy.crash_reports"]["control"]
	_ok(privacy.selected == -1, "crash reports not asked selects nothing")
	_ok(privacy.text == tr(&"settings_ui.option.unset"), "and says so in words: '%s'" % privacy.text)
	_ok((screen.get(&"_extras") as Node).get_child_count() >= 1, "privacy lists what a report holds")
	# Playtest: the session report (DESIGN.md L2209) has its own copy control, first on the page.
	screen.call(&"_show_section", &"playtest")
	var copy_report := (screen.get(&"_extras") as Node).find_child("CopyReport", true, false) as Button
	_ok(copy_report != null and tr(copy_report.text) == tr(&"settings_ui.playtest.copy_report"), "Playtest has Copy playtest report")
	_ok((screen.get(&"_page_focus") as Array).has(copy_report), "it is in the page's focus order")
	if copy_report != null:
		copy_report.pressed.emit()
		var status := screen.get(&"_status") as Label
		_ok(status != null and status.visible and status.text in [tr(&"settings_ui.playtest.report_copied"), tr(&"settings_ui.playtest.nothing_to_copy")], "pressing it says what happened")
	# Every page builds, and each focusable control has explicit Up and Down neighbours.
	for section: StringName in expected:
		screen.call(&"_show_section", section)
		for c: Control in screen.get(&"_page_focus"):
			_ok(not c.focus_neighbor_top.is_empty() and not c.focus_neighbor_bottom.is_empty(), "%s: %s has Up and Down neighbours" % [section, c.name])
	screen.call(&"_show_section", &"input")
	_ok((screen.get(&"_page_focus") as Array).size() >= 2, "the controls page offers the bindings page")
	var extras_node := screen.get(&"_extras") as Node
	var rows_node := screen.get(&"_rows_box") as Node
	_ok(extras_node.get_index() < rows_node.get_index() and String(((screen.get(&"_page_focus") as Array)[0] as Button).text) == "settings_ui.bindings.open", "the bindings button leads the controls page")
	screen.call(&"_show_section", &"access")
	_ok(extras_node.get_index() == rows_node.get_index() + 1 and rows_node.get_index() == (screen.get(&"_page_title") as Node).get_index() + 1, "other pages put extras after the rows")
	screen.call(&"_show_section", &"graphics")
	var tier: OptionButton = (screen.get(&"_rows") as Dictionary)[&"graphics.tier"]["control"]
	_ok(tier.selected == 0, "Automatic (-1) shows as the first option")
	tier.select(1)
	tier.item_selected.emit(1)
	_ok(_settings.values.get("graphics.tier") == 0, "picking the second option commits its value, not -1")
	tier.select(0)
	tier.item_selected.emit(0)
	_ok(_settings.values.get("graphics.tier") == -1, "picking Automatic commits -1")
	# Reset asks first, then resets only that page.
	screen.call(&"_on_reset_chosen", &"keep", &"input")
	_ok(not _settings.resets.has(&"input"), "keeping does not reset")
	screen.call(&"_on_reset_chosen", &"reset", &"input")
	_ok(_settings.resets.has(&"input"), "reset resets the page")
	screen.call(&"_show_section", &"access")
	_fits(screen, "settings, access page")


func _bindings_checks() -> void:
	var kb_fire := InputEventKey.new()
	kb_fire.physical_keycode = KEY_F
	_settings.binding_rows = [
		{"action": &"fire", "label": &"settings.action.fire", "group": &"sortie", "keyboard": [kb_fire], "gamepad": [], "is_hold": false, "gamepad_allowed": true},
		{"action": &"editor_undo", "label": &"settings.action.editor_undo", "group": &"editor", "keyboard": [], "gamepad": [], "is_hold": false, "gamepad_allowed": false},
	]
	var screen := _instance("settings_bindings")
	screen.call(&"setup", {})
	var slots: Dictionary = screen.get(&"_slots")
	_ok(slots.size() == 6, "two keyboard and two gamepad slots for fire, two keyboard slots for an editor action (%d)" % slots.size())
	_ok(slots.has("fire/keyboard/0") and (slots["fire/keyboard/0"] as Button).text == "F", "a bound slot shows its key")
	_ok((slots["fire/keyboard/1"] as Button).text == tr(&"settings.binding.none"), "an empty slot says so")
	_ok(not slots.has("editor_undo/gamepad/0"), "editor actions take no gamepad slot")
	_fits(screen, "bindings")
	# A lone modifier binds on release; a chord goes to rebind with its modifier (which refuses it).
	screen.call(&"_listen", &"fire", &"keyboard", 1, "Fire")
	var shift := InputEventKey.new()
	shift.physical_keycode = KEY_SHIFT
	shift.pressed = true
	screen.call(&"_input", shift)
	_ok(_settings.rebinds.is_empty(), "pressing Shift alone does not bind yet")
	var w := InputEventKey.new()
	w.physical_keycode = KEY_W
	w.shift_pressed = true
	w.pressed = true
	screen.call(&"_input", w)
	_ok(_settings.rebinds.size() == 1 and (_settings.rebinds[0]["event"] as InputEventKey).physical_keycode == KEY_W, "Shift then W is sent as one event")
	screen.call(&"_listen", &"fire", &"keyboard", 1, "Fire")
	screen.call(&"_input", shift)
	var shift_up := shift.duplicate() as InputEventKey
	shift_up.pressed = false
	screen.call(&"_input", shift_up)
	_ok(_settings.rebinds.size() == 2 and (_settings.rebinds[1]["event"] as InputEventKey).physical_keycode == KEY_SHIFT, "Shift alone binds on release")
	# Input from the other device stops listening without binding.
	screen.call(&"_listen", &"fire", &"gamepad", 0, "Fire")
	screen.call(&"_input", w)
	_ok(_settings.rebinds.size() == 2 and (screen.get(&"_listening") as Dictionary).is_empty(), "a key stops a gamepad capture")
	# A bind keeps focus on the slot that was rebound (slots are relabelled in place).
	var slot_button: Button = (screen.get(&"_slots") as Dictionary)["fire/keyboard/1"]
	slot_button.grab_focus()
	screen.call(&"_listen", &"fire", &"keyboard", 1, "Fire")
	var q := InputEventKey.new()
	q.physical_keycode = KEY_Q
	q.pressed = true
	screen.call(&"_input", q)
	await process_frame
	_ok(slot_button.get_viewport().gui_get_focus_owner() == slot_button and is_instance_valid(slot_button), "focus stays on the rebound slot")
	_settings.rebinds.clear()
	_settings.rebinds.append({})
	_settings.rebinds.append({})
	# A conflict asks; moving retries with replace.
	_settings.conflict_once = true
	screen.call(&"_listen", &"fire", &"keyboard", 1, "Fire")
	var e := InputEventKey.new()
	e.physical_keycode = KEY_E
	e.pressed = true
	screen.call(&"_input", e)
	_ok(root.get_node("Screens").call(&"modal_count") == 1, "a conflict opens a dialog")
	await process_frame
	var dialog: Node = root.get_node("Screens/Modals").get_child(0) if root.has_node("Screens/Modals") else null
	if dialog != null:
		dialog.call(&"choose", &"move")
	_ok(_settings.rebinds.size() == 4 and bool(_settings.rebinds[3]["replace"]), "moving the binding retries with replace")
	await process_frame
	_ok(slot_button.get_viewport().gui_get_focus_owner() == slot_button, "focus is back on the slot after the dialog")
	# Clearing: Delete on a slot unbinds that slot.
	var del := InputEventKey.new()
	del.physical_keycode = KEY_DELETE
	del.pressed = true
	screen.call(&"_on_slot_input", del, &"fire", &"keyboard", 0)
	_ok(_settings.unbinds.size() == 1 and _settings.unbinds[0] == [&"fire", &"keyboard", 0], "Delete clears a bound slot")
	screen.call(&"_on_reset_chosen", &"reset", &"gamepad")
	_ok(_settings.resets.has(&"gamepad"), "reset gamepad resets that device")


# --- menus ---------------------------------------------------------------------------------------

func _main_menu_checks() -> void:
	var menu := _instance("main_menu")
	var primary: Button = menu.get(&"_primary")
	var tutorial: Button = menu.get(&"_tutorial")
	var slot := {"path": "user://saves/a.bps", "slot_name": "Harbour", "act": 0, "day": 3, "played_s": 4000, "loadable": true, "is_suspend": false, "modified_unix": 1763100000}
	var empty: Array[Dictionary] = []
	menu.call(&"apply_state", {}, empty, false)
	_ok(primary.text == tr(&"menu.tutorial.start") and not tutorial.visible, "first launch: the tutorial is the first button, once")
	var slots: Array[Dictionary] = [slot]
	menu.call(&"apply_state", {}, slots, true)
	_ok(primary.text == tr(&"menu.continue").format(["Harbour"]) and tutorial.visible, "a save: continue it")
	_ok(tutorial.text == "menu.tutorial.again", "a finished tutorial is offered again")
	var suspend := slot.duplicate()
	suspend["is_suspend"] = true
	menu.call(&"apply_state", suspend, slots, true)
	_ok(primary.text == tr(&"menu.resume"), "a suspend save comes first")
	var broken := slot.duplicate()
	broken["loadable"] = false
	var broken_slots: Array[Dictionary] = [broken]
	menu.call(&"apply_state", {}, broken_slots, true)
	_ok(primary.text == tr(&"menu.campaign.start"), "an unreadable save is not offered to continue")
	var campaign_row := menu.get_node(^"Scroll/Margin/Column/Play/Campaign") as Button
	_ok(campaign_row.visible, "an unreadable save keeps the saves row, where it can be deleted")
	menu.call(&"apply_state", {}, empty, true)
	_ok(primary.text == tr(&"menu.campaign.start") and not campaign_row.visible, "after the tutorial with no saves: Start a campaign once, no saves row repeating it")
	menu.call(&"apply_state", {}, broken_slots, true)
	_ok(not primary.focus_neighbor_bottom.is_empty(), "the menu has a focus chain")
	_fits(menu, "main menu")


func _pause_checks() -> void:
	var pause := _instance("pause_menu")
	pause.call(&"describe", &"sortie", 1, true, false)
	_ok(pause.call(&"leave_kind") == &"suspend", "a hosted campaign sortie saves and leaves")
	pause.call(&"describe", &"sortie", 1, false, false)
	_ok(pause.call(&"leave_kind") == &"drop", "a co-op member leaves the operation")
	pause.call(&"describe", &"sortie", 2, false, false)
	_ok(pause.call(&"leave_kind") == &"lose", "a skirmish loses its result")
	pause.call(&"describe", &"splash", 5, false, false)
	_ok(pause.call(&"leave_kind") == &"splash", "a splash returns to the dry dock")
	pause.call(&"describe", &"chart", -1, false, true)
	_ok(pause.call(&"leave_kind") == &"chart_save", "the chart saves when it may")
	pause.call(&"describe", &"chart", -1, false, false)
	_ok(pause.call(&"leave_kind") == &"chart_lose", "the chart warns when it may not")
	_fits(pause, "pause menu")


func _credits_checks() -> void:
	var credits := _instance("credits")
	var lic: Array[Dictionary] = [{"name": "Godot Engine", "licence": "MIT", "text": "Copyright"}, {"name": "Fonts", "licence": "OFL-1.1", "text": "OFL"}]
	credits.call(&"show_data", lic, {"version": "0.1.0", "build_id": "local"})
	var list: ItemList = credits.get(&"_list")
	_ok(list.item_count == 2 and list.is_selected(0), "licences listed, the first shown")
	_ok((credits.get(&"_text") as RichTextLabel).text == "Copyright", "the chosen licence's text shows")
	_fits(credits, "credits")
	var none: Array[Dictionary] = []
	credits.call(&"show_data", none, {})
	_ok((credits.get(&"_text") as RichTextLabel).text == tr(&"credits.no_licences"), "an empty licence list says so")


func _campaign_checks() -> void:
	var slots_screen := _instance("campaign_slots")
	var none: Array[Dictionary] = []
	slots_screen.call(&"show_slots", none)
	_ok(String((slots_screen.get(&"_focus_first") as Button).text) == "campaign_menu.slots.start_first", "no saves: one start button")
	var saves: Array[Dictionary] = [
		{"path": "a", "slot_name": "Alpha", "loadable": true, "is_suspend": false, "act": 0, "day": 2},
		{"path": "b", "slot_name": "Bravo", "loadable": false, "is_suspend": false, "problem": {"key": &"campaign_menu.slots.unreadable"}},
		{"path": "s", "slot_name": "Alpha", "loadable": true, "is_suspend": true},
	]
	slots_screen.call(&"show_slots", saves)
	var rows: Array = slots_screen.get(&"_row_buttons")
	_ok(rows.size() == 3, "one row per save")
	_ok(rows[0][0] != null and String((rows[0][0] as Button).text) == "campaign_menu.slots.resume", "the suspend slot is first")
	_ok(rows[2][0] == null, "an unreadable save has no load button")
	_ok(slots_screen.get(&"_focus_first") == rows[0][0], "the suspend slot takes focus")
	_fits(slots_screen, "campaign saves")

	var form := _instance("campaign_new")
	var content := root.get_node("Content")
	var presets: Array[Dictionary] = []
	for p: Variant in content.call(&"difficulty_presets"):
		presets.append(p)
	var ranges: Dictionary = content.call(&"difficulty_ranges")
	var starters: Array[Dictionary] = [
		{"path": "stock:starters/patrol_15", "name": "Patrol boat", "length_m": 15.0, "displacement_t": 40.0, "cost": 400},
		{"path": "stock:starters/fac_22", "name": "Fast attack craft", "length_m": 22.0, "displacement_t": 120.0, "cost": 1000},
		{"path": "stock:starters/insertion_26", "name": "Insertion craft", "length_m": 26.0, "displacement_t": 140.0, "cost": 1100},
	]
	form.call(&"build", presets, ranges, starters, 2)
	_ok(form.call(&"matching_preset") == &"normal", "Normal is chosen first")
	var preset_ids: Array = (form.get(&"_preset_buttons") as Dictionary).keys()
	_ok(preset_ids == [&"easy", &"normal", &"hard"], "presets read easiest to hardest (%s)" % str(preset_ids))
	var name_field := form.get(&"_name") as Control
	_ok(name_field.get_node_or_null(name_field.focus_neighbor_bottom) == (form.get(&"_preset_buttons") as Dictionary)[&"normal"], "Down from the name lands on the chosen preset")
	var opts: Dictionary = form.call(&"options")
	_ok(opts["slot_name"] == tr(&"campaign_menu.new.default_name").format([3]), "the default name counts the saves")
	_ok(opts.get("starter") == "stock:starters/fac_22", "the 22 m fast attack craft is the first hull")
	_ok(int(opts["difficulty"]["hull_count"]) == 300 and int(opts["seed"]) == 0, "Normal values and a random seed")
	var sliders: Dictionary = form.get(&"_sliders")
	(sliders["economy"] as HSlider).value = 1.4
	_ok(form.call(&"matching_preset") == &"", "moving a slider off the presets is Custom")
	_ok((form.get(&"_custom_label") as Label).visible, "Custom is shown")
	form.call(&"apply_preset", &"hard")
	_ok(form.call(&"matching_preset") == &"hard" and int(form.call(&"difficulty")["clock_days"]) == 32, "a preset sets all six sliders")
	_ok(name_field.get_node_or_null(name_field.focus_neighbor_bottom) == (form.get(&"_preset_buttons") as Dictionary)[&"hard"], "and follows a new choice")
	_ok(form.call(&"value_text", "ai_reaction_s", 1.2).contains("1.2"), "reaction time reads in seconds")
	var no_starters: Array[Dictionary] = []
	var no_presets: Array[Dictionary] = []
	form.call(&"build", no_presets, {}, no_starters, 0)
	opts = form.call(&"options")
	_ok(not opts.has("starter") and form.call(&"matching_preset") == &"normal", "unreadable tables fall back to the spec and the default starter")
	form.call(&"build", presets, ranges, starters, 2)
	_fits(form, "campaign setup")


## At 200% UI scale the logical viewport is 960 x 540 (L1714: no clipping at 200%). Screens are
## parked here and laid out at that size at the end; no top-level part may need more room.
func _fits(screen: Node, what: String) -> void:
	_parked.append([screen, what])


func _measure_parked() -> void:
	for entry: Array in _parked:
		var screen := entry[0] as Control
		screen.set_anchors_preset(Control.PRESET_TOP_LEFT)
		screen.position = Vector2.ZERO
		screen.size = Vector2(960.0, 540.0)
	await process_frame
	await process_frame
	for entry: Array in _parked:
		var screen := entry[0] as Control
		var need := Vector2.ZERO
		for child: Node in screen.get_children():
			if child is Control:
				need = need.max((child as Control).size)
		_ok(need.x <= 960.5 and need.y <= 540.5, "%s fits 960 x 540 at 200%% scale (lays out at %s)" % [entry[1], str(need)])
	for entry: Array in _parked:
		(entry[0] as Node).queue_free()
	_parked.clear()


## Every settings page at 200% (960 x 540 logical): the page scroll shows no horizontal bar, so a
## row wider than the scroll area would clip without failing the screen-level fit check.
func _settings_pages_fit() -> void:
	var screen := _instance("settings") as Control
	screen.call(&"setup", {})
	screen.set_anchors_preset(Control.PRESET_TOP_LEFT)
	screen.position = Vector2.ZERO
	screen.size = Vector2(960.0, 540.0)
	var scroll := screen.get_node(^"Margin/Column/Body/PageScroll") as ScrollContainer
	var page := screen.get_node(^"Margin/Column/Body/PageScroll/Page") as Control
	for section: StringName in screen.get(&"_sections"):
		screen.call(&"_show_section", section)
		await process_frame
		await process_frame
		var need := page.get_combined_minimum_size().x
		var room := scroll.size.x - scroll.get_v_scroll_bar().get_combined_minimum_size().x
		_ok(need <= room + 0.5, "settings page %s fits at 200%% (needs %.0f of %.0f px)" % [section, need, room])
	screen.queue_free()


func _setup_checks() -> void:
	var skirmish := _instance("skirmish_setup")
	var content := root.get_node("Content")
	var templates: Array[Dictionary] = []
	for t: Variant in content.call(&"mission_templates"):
		templates.append(t)
	var presets: Array[Dictionary] = []
	for p: Variant in content.call(&"difficulty_presets"):
		presets.append(p)
	# This build ships Act 1 alone (DESIGN.md L2283): Content names one cap, and the act list has one row.
	var shipped: Array[float] = []
	for c: float in content.call(&"act_caps") as PackedFloat32Array:
		shipped.append(c)
	_ok(shipped == [150.0], "Content offers Act 1's cap alone")
	skirmish.call(&"fill", templates, presets, shipped)
	_ok((skirmish.get(&"_act") as OptionButton).item_count == 1, "skirmish offers Act 1 alone")
	skirmish.call(&"fill", templates, presets, [] as Array[float])
	_ok((skirmish.get(&"_act") as OptionButton).item_count == 1, "no caps still offers Act 1 alone")
	# A later release ships more acts: one row per cap.
	var caps: Array[float] = [150.0, 400.0, 900.0, 1800.0]
	skirmish.call(&"fill", templates, presets, caps)
	_ok((skirmish.get(&"_act") as OptionButton).item_count == 4, "one act row per shipped cap")
	var lobby := _instance("lobby")
	lobby.call(&"fill_choices", templates, presets, shipped)
	_ok((lobby.get(&"_act") as OptionButton).item_count == 1, "the lobby's skirmish offers Act 1 alone")
	lobby.queue_free()
	var picker: Node = skirmish.get(&"_picker")
	var designs: Array[Dictionary] = [
		{"path": "stock:starters/fac_22", "stock": true, "info": {"name": "Fast attack craft", "length_m": 22.0, "displacement_t": 120.0}},
		{"path": "user://blueprints/big.bpn", "stock": false, "info": {"name": "Big", "length_m": 60.0, "displacement_t": 600.0}},
		{"path": "user://blueprints/long.bpn", "stock": false, "info": {"name": "Long", "length_m": 95.0, "displacement_t": 1200.0}},
	]
	picker.call(&"set_entries", designs)
	_ok(picker.call(&"selected_path") == "stock:starters/fac_22", "the 22 m starter is chosen first")
	_ok(not picker.call(&"blocked"), "it fits Act 1")
	skirmish.call(&"_refresh_start")
	var start_ready := not (skirmish.get(&"_start") as Button).disabled
	_ok(start_ready == (not templates.is_empty()), "Start is ready with a template and a fitting design")
	picker.call(&"select_entry", 1)
	_ok(picker.call(&"blocked"), "600 t is over the 150 t cap")
	(skirmish.get(&"_act") as OptionButton).select(2)
	skirmish.call(&"_on_act")
	_ok(not picker.call(&"blocked"), "600 t fits Act 3")
	picker.call(&"select_entry", 2)
	_ok(picker.call(&"blocked"), "95 m is longer than any player hull")
	_editor.issues["user://blueprints/big.bpn"] = [{"text": {"key": &"skirmish.failed"}, "fatal": true}]
	(picker.get(&"_issue_cache") as Dictionary).clear()
	picker.call(&"select_entry", 1)
	_ok(picker.call(&"blocked"), "a fatal file issue blocks the design")
	var setup: Dictionary = skirmish.call(&"setup_dict")
	for k: String in ["template", "act", "enemy_per_player", "difficulty", "weather", "sea_state", "time_of_day", "seed", "blueprint"]:
		_ok(setup.has(k), "skirmish setup has " + k)
	_ok(int(setup["enemy_per_player"]) == 5 and int(setup["sea_state"]) == 3 and int(setup["time_of_day"]) == 1, "defaults: 5 hulls, sea state 3, dusk")
	_ok(not (setup["difficulty"] as Dictionary).has("name"), "the difficulty sent is a DifficultyDict")
	_fits(skirmish, "skirmish setup")
	var needs: String = skirmish.call(&"needs_text", PackedStringArray(["boarding_kit", "rhib_davit"]))
	_ok(needs.contains(tr(&"notes.tag_word.boarding_kit")), "needs name the fittings")

	var Picker: GDScript = load(SCREENS + "design_picker.gd")
	_ok(Picker.child_folder("", {"path": "user://blueprints/ships/", "name": "ships"}) == "ships", "a user:// folder browses relative")
	_ok(Picker.child_folder("", {"path": "stock:starters", "name": "starters"}) == "stock:starters", "a stock folder keeps its prefix")
	_ok(Picker.child_folder("ships", {"path": "/abs/x", "name": "small"}) == "/abs/x", "a relative-looking path is used as given")
	_ok(Picker.child_folder("ships", {"path": "file://x/small", "name": "small"}) == "ships/small", "an unknown scheme joins the name")

	var sandbox := _instance("sandbox_setup")
	var classes: Array[Dictionary] = [{"id": &"patrol", "name": &"content.role.patrol_boat", "length_m": 18.0, "tonnage_t": 60.0, "blueprint": "stock:classes/patrol"},
		# A long name must not widen the target row past the window at 200%.
		{"id": &"long", "name": "Fleet replenishment oiler with a very long class name for layout", "length_m": 88.0, "tonnage_t": 1700.0, "blueprint": "stock:classes/long"}]
	sandbox.call(&"set_choices", classes, designs)
	var targets: Array = sandbox.call(&"targets")
	_ok(targets.size() == 1 and targets[0]["blueprint"] == "stock:classes/patrol" and targets[0]["hostile"] == true and targets[0]["count"] == 2,
		"the sandbox starts with two hostile ships of the first class")
	sandbox.call(&"_on_add")
	_ok((sandbox.call(&"targets") as Array).size() == 2, "a target row can be added")
	var rows: Array = sandbox.get(&"_rows")
	sandbox.call(&"_remove_row", rows[0], true)
	_ok((sandbox.call(&"targets") as Array).size() == 1, "and removed")
	var none: Array[Dictionary] = []
	sandbox.call(&"set_choices", none, none)
	_ok((sandbox.call(&"targets") as Array).is_empty() and (sandbox.get(&"_empty_label") as Label).visible, "no ships: the sandbox says it opens with yours alone")
	var sd: Dictionary = sandbox.call(&"setup_dict")
	_ok(sd.has("targets") and sd.has("blueprint") and sd.has("seed"), "sandbox setup has its keys")
	sandbox.call(&"set_choices", classes, designs)
	_fits(sandbox, "sandbox setup")
