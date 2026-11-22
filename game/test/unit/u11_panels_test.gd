extends SceneTree
## Headless checks of the dry dock's feedback panels (units ui-editor-gauges, ui-editor-notes, ui-editor-fit,
## ui-editor-trials, ui-editor-splash). Run from game/ with a throwaway user folder:
##   HOME=/tmp/bp-u11 godot --headless --path . -s res://test/unit/u11_panels_test.gd
## Checks: the gauge strip takes every check and limit from the dictionary (Caution and Fail badges, GM Fail
## at zero, an unmeasured Complexity has no check and no zero), draws named gauges stale only after the hold,
## and reads packed arrays; the coverage share; the splash options round trip through
## `Editor.set_splash_options` (missing keys keep their value, a bad target is ignored); scoped values as
## text; the report card opens on a card and closes on Esc; Ship's Notes lists hand-built notes in order
## with one Fix each and opens no preview by itself; Showing every right-dock tab changes nothing in the
## design. Exits 1 on any failure.

const DOCK_PATH := "res://ui/editor/dry_dock.tscn"
const STRIP_PATH := "res://ui/editor/gauge_strip.gd"
const BAR_PATH := "res://ui/editor/splash_bar.gd"

var editor: Node
var failures := 0


func _initialize() -> void:
	_run.call_deferred()


func check(ok: bool, what: String) -> void:
	if not ok:
		failures += 1
		push_error("u11_panels_test: " + what)
	else:
		print("u11_panels_test: ok ", what)


func frames(n: int) -> void:
	for i in n:
		await process_frame


func _run() -> void:
	editor = root.get_node("Editor")
	var issues: Array = editor.call(&"open", Bp.DOCK_SANDBOX, {"blueprint": "starters/fac22.bp.ron"})
	for i: Dictionary in issues:
		check(not bool(i.get("fatal", false)), "open: " + str(issues))
	var dock := (load(DOCK_PATH) as PackedScene).instantiate() as Control
	root.add_child(dock)
	await frames(60)
	var rev0 := int(editor.call(&"rev"))

	# --- gauge strip ---
	var strip_script := load(STRIP_PATH) as GDScript
	check(is_equal_approx(strip_script.call(&"clear_share", PackedFloat32Array([1.0, 0.5, 0.49, 0.0])), 0.5), "clear share counts sectors at 50% or more")
	check(strip_script.call(&"clear_share", PackedFloat32Array()) == 0.0, "no sectors, no share")
	var strip := dock.get_node(^"Scroll/Frame/Gauges")
	var live: Dictionary = editor.call(&"gauges")
	check(not live.is_empty() and live.has("float"), "live gauges arrive")
	var g := live.duplicate(true)
	(g["float"] as Dictionary)["check"] = Bp.CHECK_FAIL
	(g["float"] as Dictionary)["gm_m"] = -0.1
	(g["power"] as Dictionary)["check"] = Bp.CHECK_CAUTION
	(g["complexity"] as Dictionary)["measured"] = false
	g["stale"] = PackedStringArray(["sight"])
	strip.call(&"show_strip", g, {}, false)
	var cells: Dictionary = strip.get(&"_cells")
	var fail_word := String(root.get_node("Tx").call(&"word", &"check", Bp.CHECK_FAIL))
	var caution_word := String(root.get_node("Tx").call(&"word", &"check", Bp.CHECK_CAUTION))
	check(String((cells[&"float"] as Control).accessibility_name).contains(fail_word), "Float shows Fail from its check")
	check(String((cells[&"power"] as Control).accessibility_name).contains(caution_word), "Power shows Caution from its check")
	var cx: Dictionary = (cells[&"complexity"] as Object).get(&"_spec")
	check(int(cx["check"]) == -1, "unmeasured Complexity has no check")
	check(not String((cells[&"complexity"] as Control).accessibility_name).contains(" 0,"), "unmeasured Complexity is not zero")
	check(not bool((cells[&"sight"] as Object).get(&"_stale")), "a stale gauge waits for the hold before it is drawn stale")
	strip.call(&"show_strip", g, {}, true)
	check(bool((cells[&"sight"] as Object).get(&"_stale")), "a stale gauge past the hold is drawn stale")
	check(not bool((cells[&"speed"] as Object).get(&"_stale")), "a fresh gauge is not stale")
	var fl: Dictionary = (cells[&"float"] as Object).get(&"_spec")
	var gm_col: Dictionary = (fl["cols"] as Array)[0]
	check(float(gm_col["warning_lo"]) == 0.0 and is_equal_approx(float(gm_col["band_lo"]), float((g["float"] as Dictionary)["gm_caution_low"])), "GM limits come from the dictionary")
	strip.call(&"show_strip", {}, {}, false)
	check(int(((cells[&"float"] as Object).get(&"_spec") as Dictionary)["check"]) == 4, "no reading draws Updating")
	strip.call(&"_refresh")

	# --- splash options ---
	editor.call(&"set_splash_options", {"water": 6, "target": Bp.SPLASH_TARGET_PATROL_BOAT})
	var o: Dictionary = editor.call(&"splash_options")
	check(int(o["water"]) == 6 and int(o["target"]) == Bp.SPLASH_TARGET_PATROL_BOAT, "options set")
	editor.call(&"set_splash_options", {"water": 0})
	o = editor.call(&"splash_options")
	check(int(o["water"]) == 0 and int(o["target"]) == Bp.SPLASH_TARGET_PATROL_BOAT, "a missing key keeps its value")
	editor.call(&"set_splash_options", {"target": 9})
	o = editor.call(&"splash_options")
	check(int(o["target"]) == Bp.SPLASH_TARGET_PATROL_BOAT, "a bad target is ignored")
	var panel := dock.get_node(^"SplashOptions/Options")
	panel.call(&"_load")
	var water_buttons: Array = panel.get(&"_water")
	check((water_buttons[0] as Button).button_pressed, "the popup marks the chosen water")
	(water_buttons[1] as Button).button_pressed = true
	(water_buttons[1] as Button).pressed.emit()
	check(int(editor.call(&"splash_options")["water"]) == 3, "picking Beaufort 4 sets it")
	editor.call(&"set_splash_options", {"water": 3, "target": Bp.SPLASH_TARGET_DRONES})

	# --- splash bar ---
	var bar_script := load(BAR_PATH) as GDScript
	check(String(bar_script.call(&"value_text", {"type": &"bool", "value": true})) == tr(&"editor_splash.value.on"), "a true scope value reads On")
	check(not String(bar_script.call(&"value_text", {"type": &"scalar", "value": 12.5})).is_empty(), "a scalar scope value has text")
	check(String(bar_script.call(&"value_text", {"type": &"bogus"})) == tr(&"editor_splash.value.none"), "an unknown scope value says so")
	var bar := dock.get_node(^"SplashUi/Bar")
	check((bar.get(&"_rewind") as Button).disabled and (bar.get(&"_end") as Button).disabled, "outside a splash the bar does nothing")

	# --- report card ---
	var report := dock.get_node(^"ReportLayer/Report") as Control
	check(not report.visible, "the report starts hidden")
	report.call(&"show_card", {"duration_s": 60.0, "rounds_fired": 0, "hit_rate": 0.0, "targets_destroyed": 0, "time_to_first_hit_s": -1.0, "damage_taken": 0.0, "peak_heel_deg": 3.0, "score": 0.0, "tip": {}})
	check(report.visible, "a card opens the report")
	var esc := InputEventAction.new()
	esc.action = &"ui_cancel"
	esc.pressed = true
	report.call(&"_unhandled_input", esc)
	check(not report.visible, "Esc closes the report")

	# --- Ship's Notes ---
	var notes := dock.get_node(^"Scroll/Frame/Body/RightDock/Tabs/Notes")
	var hand: Array[Dictionary] = [
		{"index": 0, "tier": Bp.NOTE_CANNOT_FLOAT_MOVE_OR_POWER, "text": {"key": &"notes.float_fail.gm", "args": [{"num": -0.1, "unit": &"m"}]}, "part": -1, "affected_parts": 0, "has_fix": true, "flips_tag": false, "score_effect": 0.0, "fix_label": {"key": &"dock.fix.bulkhead", "args": []}},
		{"index": 1, "tier": Bp.NOTE_ADVISORY, "text": {"key": &"notes.float_fail.gm", "args": [{"num": 0.1, "unit": &"m"}]}, "part": 3, "affected_parts": 1, "has_fix": false, "flips_tag": false, "score_effect": 0.0, "fix_label": {}},
	]
	notes.set(&"_notes", hand)
	notes.call(&"render", true)
	var found_list: Node = notes.find_child("List", true, false)
	check(found_list != null and found_list.get_child_count() == 2, "two notes, two rows")
	var fix_buttons := 0
	for b: Node in found_list.find_children("*", "Button", true, false):
		if String(b.get_meta(&"focus_key", "")).begins_with("fix:"):
			fix_buttons += 1
	check(fix_buttons == 1, "one Fix for the note that has one")
	check((notes.get(&"_preview") as Dictionary).is_empty(), "no preview until Fix is pressed")
	notes.call(&"_refresh")

	# Every right-dock tab shown: nothing may change the design.
	var right := dock.get_node(^"Scroll/Frame/Body/RightDock/Tabs") as TabContainer
	for t in right.get_tab_count():
		right.current_tab = t
		await frames(3)
	right.current_tab = 0
	await frames(3)
	check(int(editor.call(&"rev")) == rev0, "showing the panels changed nothing (rev %d -> %d)" % [rev0, int(editor.call(&"rev"))])

	# A real Fix: preview, then accept as one undo step; the preview closes with it.
	notes.call(&"_refresh")
	await frames(2)
	var real: Array = editor.call(&"notes")
	var fix_index := -1
	for n: Dictionary in real:
		if bool(n["has_fix"]):
			fix_index = int(n["index"])
			break
	if fix_index >= 0:
		notes.call(&"_preview_note", fix_index)
		check(not (notes.get(&"_preview") as Dictionary).is_empty(), "Fix opens a preview")
		var depth := int(Dictionary(editor.call(&"undo_state"))["depth"])
		notes.call(&"_accept_note", fix_index)
		await frames(2)
		check(int(Dictionary(editor.call(&"undo_state"))["depth"]) == depth + 1, "accepting a Fix is one undo step")
		check((notes.get(&"_preview") as Dictionary).is_empty(), "accepting closes the preview")
		editor.call(&"undo")
		await frames(2)
	else:
		print("u11_panels_test: no note with a Fix on this design; Fix accept not exercised")

	dock.queue_free()
	editor.call(&"close")
	await frames(2)
	print("u11_panels_test: %d failures" % failures)
	quit(1 if failures > 0 else 0)
