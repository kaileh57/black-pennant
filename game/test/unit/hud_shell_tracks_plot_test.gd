extends SceneTree
## Headless checks for the sortie HUD shell, the track list, card and bridge overlay, the plot
## table panel and the kill cam, on hand-built data. Units: ui-hud-shell, ui-hud-tracks,
## ui-hud-plot. Run from game/:
##   godot --headless --path . -s res://test/unit/hud_shell_tracks_plot_test.gd
## Exits 1 on any failure. No session runs; the facades answer as when idle. The windowed end-to-end
## checks (keys, clicks and gamepad buttons in a live skirmish) are res://test/capture/u6_capture.tscn
## with --checks=1.

var _failures := 0
var _checks := 0
var _frame := 0
var _hud: Node
var _views: Array[int] = []


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame == 2:
		_hud = (load("res://ui/hud/sortie_hud.tscn") as PackedScene).instantiate()
		root.add_child(_hud)
		_hud.connect(&"view_switched", func(v: int) -> void: _views.append(v))
	if _frame == 5:
		_shell_checks()
		_track_list_checks()
		_overlay_checks()
		_card_checks()
		_plot_checks()
		_kill_cam_checks()
		_hud.queue_free()
	if _frame < 8:
		return false
	print("hud_shell_tracks_plot_test: %d checks, %d failures" % [_checks, _failures])
	quit(1 if _failures > 0 else 0)
	return true


func _expect(ok: bool, what: String) -> void:
	_checks += 1
	if not ok:
		_failures += 1
		push_error("hud_shell_tracks_plot_test: " + what)


func _node(path: String) -> Node:
	return _hud.get_node(path)


# --- shell ---------------------------------------------------------------------------------------

func _shell_checks() -> void:
	var sink: Control = _hud.call(&"default_focus")
	_expect(sink != null and sink.focus_mode == Control.FOCUS_NONE, "the HUD offers no focusable default, so keys stay with the sortie")
	var z := InputEventKey.new()
	z.physical_keycode = KEY_Z
	z.pressed = true
	_expect(not bool(_hud.call(&"shares_input_with_dock", z)), "outside a Splash no key belongs to the dry dock")
	# Views.
	_hud.call(&"set_view", Bp.VIEW_PLOT)
	_expect(int(_hud.call(&"view")) == Bp.VIEW_PLOT, "set_view switches to the plot")
	_expect((_node("Frame/View/PlotTable") as Control).visible, "the plot table shows in the plot view")
	_expect(not (_node("Frame/View/BridgeOverlay") as Control).visible, "the bridge overlay hides under the plot")
	_hud.call(&"toggle_view")
	_expect(int(_hud.call(&"view")) == Bp.VIEW_BRIDGE, "toggle_view comes back to the bridge view")
	_expect(_views == [Bp.VIEW_PLOT, Bp.VIEW_BRIDGE], "view_switched reports each switch once: %s" % str(_views))
	_hud.call(&"set_view", 7)
	_expect(int(_hud.call(&"view")) == Bp.VIEW_BRIDGE, "an unknown view is ignored")
	# Layout: the bridge view's overlay covers the frame (the sea shows between the docks' clusters),
	# the docks' open space passes clicks through, and every cluster is listed for the overlay.
	var view_root := _node("Frame/View") as Control
	_expect(is_equal_approx(view_root.anchor_right, 1.0) and is_equal_approx(view_root.anchor_bottom, 1.0), "the bridge view's overlay covers the whole frame")
	for dock: String in ["Frame/Docks/LeftDock", "Frame/Docks/RightDock"]:
		_expect((_node(dock + "/Column/Fill") as Control).mouse_filter == Control.MOUSE_FILTER_IGNORE, dock + " leaves its open space to the sea")
	var listed: Array = _hud.call(&"clusters")
	for path: String in ["Frame/Docks/LeftDock/Column/TrackList", "Frame/Docks/RightDock/Column/BridgeBoard", "Frame/Docks/LeftDock/Column/WeaponsPanel", "Frame/Docks/Centre/Margin/Column/Bottom/Row/HelmCluster", "Frame/Docks/Centre/Margin/Column/Top/Middle/AlertStrip"]:
		_expect(listed.has(_node(path)), "the overlay keeps clear of " + path)
	var strip := _node("Frame/Docks/Centre/Margin/Column/Top/Middle/AlertStrip")
	strip.call(&"set_max_rows", 2)
	strip.call(&"show_strip", {"shown": [
		{"id": 1, "priority": 1, "state": 0, "text": {"key": &"hud_alerts.strip.access", "args": []}},
		{"id": 2, "priority": 0, "state": 0, "text": {"key": &"hud_alerts.strip.access", "args": []}},
		{"id": 3, "priority": 0, "state": 0, "text": {"key": &"hud_alerts.strip.access", "args": []}},
	], "more": 1, "next_ack": 1})
	var rows_shown := 0
	for r: Node in strip.get_node("Margin/Column/Rows").get_children():
		rows_shown += 1 if (r as Control).visible else 0
	_expect(rows_shown == 2 and (strip.get_node("Margin/Column/More") as Button).text.contains("2"), "a short centre shows fewer alert rows and counts the rest into +N")
	strip.call(&"set_max_rows", 5)
	strip.call(&"show_strip", {})
	# Roles: no groups on an idle HUD, then two groups.
	_expect((_hud.call(&"_role_for", &"take_group_1") as Array).is_empty(), "a group key does nothing when the hull has no such group")
	_expect(_hud.call(&"_role_for", &"take_helm") == [Bp.ROLE_HELM, 0], "H takes the helm")
	_expect(_hud.call(&"_role_for", &"take_sensors") == [Bp.ROLE_SENSORS, 0], "5 takes sensors")
	_hud.set(&"_groups", [0, 2] as Array[int])
	_hud.set(&"_current_group", 0)
	_expect(_hud.call(&"_role_for", &"take_group_3") == [Bp.ROLE_WEAPON_GROUP, 2], "3 takes weapons group 3 (index 2)")
	_expect((_hud.call(&"_role_for", &"take_group_2") as Array).is_empty(), "2 does nothing when group 2 does not exist")
	_hud.call(&"_step_group", 1)
	_expect(int(_hud.call(&"current_group")) == 2, "the next-group key moves to the next existing group")
	_expect(_hud.call(&"_role_for", &"take_group_current") == [Bp.ROLE_WEAPON_GROUP, 2], "F takes the current group")
	_hud.call(&"_step_group", 1)
	_expect(int(_hud.call(&"current_group")) == 0, "the next-group key wraps")
	_hud.call(&"_step_group", -1)
	_expect(int(_hud.call(&"current_group")) == 2, "the previous-group key wraps back")
	# Stations.
	_hud.call(&"open_station", &"log")
	var station := _node("Frame/Docks/Centre/Margin/Column/Middle/Station") as Control
	_expect(station.visible and (_node("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Log") as Control).visible, "the log station opens")
	_hud.call(&"open_station", &"damage")
	_expect(not (_node("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Log") as Control).visible and (_node("Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Damage") as Control).visible, "one station at a time")
	_hud.call(&"open_station", &"damage")
	_expect(not station.visible, "the same station button closes it")
	_hud.call(&"open_station", &"nonsense")
	_expect(not station.visible, "an unknown station is ignored")
	# Dock swap follows the setting.
	var before: Variant = root.get_node("Settings").call(&"get_value", &"hud.swap_docks")
	root.get_node("Settings").call(&"set_value", &"hud.swap_docks", true)
	_hud.call(&"_apply_swap")
	_expect(_node("Frame/Docks").get_child(0).name == &"RightDock", "the swap setting puts the Bridge Board on the left")
	root.get_node("Settings").call(&"set_value", &"hud.swap_docks", false)
	_hud.call(&"_apply_swap")
	_expect(_node("Frame/Docks").get_child(0).name == &"LeftDock", "the track list returns to the left")
	if before is bool:
		root.get_node("Settings").call(&"set_value", &"hud.swap_docks", before)
	# Holds never stick: releasing everything with nothing held sends nothing and keeps state clean.
	_hud.call(&"_release_all_input")
	_expect((_hud.get(&"_roles_down") as Dictionary).is_empty() and not bool(_hud.get(&"_trigger")), "release clears role keys and the trigger")


# --- track list ----------------------------------------------------------------------------------

static func _track(id: int, stance: int, extra: Dictionary = {}) -> Dictionary:
	var t := {
		"id": id, "label": "%04d" % id, "stance": stance, "domain": Bp.DOMAIN_SURFACE, "rung": 1, "grade": 1,
		"range_m": 1000.0 * id, "bearing_deg": 10.0 * id, "last_known": false, "designated": false,
		"inbound": false, "selected": false, "vessel": 100 + id, "name": "Track %04d" % id, "trend": -1,
		"stance_reason": {}, "rung_reason": {}, "class_name": {}, "candidates": 3, "sources": 1 | 8,
		"course_deg": 90.0, "speed_kn": 12.0, "alt_m": 0.0, "cpa_m": 500.0, "cpa_s": 60.0, "age_s": 0.0,
		"engaged_by": 0, "envelope_m": 0.0, "emitting": false,
	}
	t.merge(extra, true)
	return t


func _rows_of(table: Tree) -> Array[int]:
	var out: Array[int] = []
	var item := table.get_root().get_first_child()
	while item != null:
		out.append(int(item.get_metadata(0)))
		item = item.get_next()
	return out


func _track_list_checks() -> void:
	var list := _node("Frame/Docks/LeftDock/Column/TrackList")
	var table := list.get_node("Margin/Column/Table") as Tree
	var counts := list.get_node("Margin/Column/Counts") as Label
	var empty := list.get_node("Margin/Column/Empty") as Label
	list.call(&"show_rows", [] as Array[Dictionary], {}, true)
	_expect(empty.visible and not table.visible, "an empty picture shows the waiting state, not an empty table")
	var rows: Array[Dictionary] = [_track(3, Bp.STANCE_HOSTILE, {"designated": true}), _track(1, Bp.STANCE_SUSPECT), _track(2, Bp.STANCE_HOSTILE, {"inbound": true}), _track(4, Bp.STANCE_NEUTRAL, {"last_known": true})]
	list.call(&"show_rows", rows, {})
	_expect(_rows_of(table) == [3, 1, 2, 4], "rows follow the order Hud gives: %s" % str(_rows_of(table)))
	_expect(counts.text.contains("0003"), "the count line names the designated track: %s" % counts.text)
	_expect(not counts.text.begins_with("4"), "last-known marks are not counted as contacts: %s" % counts.text)
	var first := table.get_root().get_first_child()
	rows = [rows[2], rows[0], rows[1]]
	list.call(&"show_rows", rows, {})
	_expect(_rows_of(table) == [2, 3, 1], "a new order moves rows and drops the gone: %s" % str(_rows_of(table)))
	var three := table.get_root().get_child(1)
	_expect(three == first, "rows are reused by track number, not rebuilt")
	_expect(table.get_root().get_child(0).get_tooltip_text(1).contains("Track 0002"), "each row carries the track's accessible name")
	list.call(&"show_rows", [_track(5, Bp.STANCE_HOSTILE), _track(5, Bp.STANCE_HOSTILE), _track(-1, 2)] as Array[Dictionary], {})
	_expect(_rows_of(table) == [5], "duplicate and malformed rows are dropped")


# --- overlay -------------------------------------------------------------------------------------

func _overlay_checks() -> void:
	var overlay := _node("Frame/View/BridgeOverlay") as Control
	overlay.call(&"set_tracks", [_track(1, Bp.STANCE_HOSTILE, {"selected": true}), _track(2, Bp.STANCE_SUSPECT), _track(3, Bp.STANCE_HOSTILE)] as Array[Dictionary])
	var o := overlay.get_global_transform().origin
	# Tracks 2 and 3 sit on top of each other; track 1 is selected and far away; track 3 ranks above 2.
	var markers := PackedFloat32Array([
		1, o.x + 100.0, o.y + 100.0, 1, 3000, 3,
		2, o.x + 300.0, o.y + 300.0, 1, 2000, 4,
		3, o.x + 302.0, o.y + 304.0, 1, 4000, 3,
		9, o.x + 50.0, o.y + 50.0, 1, 100, 0,
	])
	overlay.call(&"set_markers", markers)
	var laid: Array = overlay.call(&"layout_entries")
	var entries: Array = laid[0]
	var shown: Array = laid[1]
	_expect(entries.size() == 3, "markers of tracks the picture lacks are skipped")
	_expect(int(entries[0]["id"]) == 1, "the selected track ranks first")
	_expect(int(entries[1]["id"]) == 3 and int(entries[2]["id"]) == 2, "then by rank, then range")
	_expect(bool(shown[0]) and bool(shown[1]) and not bool(shown[2]), "an overlapping lower-ranked label hides: %s" % str(shown))
	markers = PackedFloat32Array([2, o.x - 5000.0, o.y + 200.0, 0, 9000, 4])
	overlay.call(&"set_markers", markers)
	laid = overlay.call(&"layout_entries")
	entries = laid[0]
	var p: Vector2 = entries[0]["pos"]
	_expect(not bool(entries[0]["on"]) and p.x >= 27.0 and Vector2(entries[0]["edge"]).x < 0.0, "an off-view track sits at the edge, pointing out: %s" % str(entries[0]))


# --- card ----------------------------------------------------------------------------------------

func _card_checks() -> void:
	var card := _node("Frame/Docks/LeftDock/Column/TrackCard")
	var title := card.get_node("Scroll/Column/Header/Title") as Label
	var facts := card.get_node("Scroll/Column/Facts") as Label
	card.call(&"show_track", _track(412, Bp.STANCE_HOSTILE, {"emitting": true}), {"track": 412, "candidates": [{"class_id": &"kestrel", "name": "Kestrel", "domain": 0, "civilian": false, "own_side": false}], "evidence": [], "stance_reason": {}})
	_expect(title.text.contains("0412"), "the card title names the track: %s" % title.text)
	_expect(facts.text.count("\n") >= 3, "the card lists bearing, course, approach and quality: %s" % facts.text)
	var id_pick := card.get_node("Scroll/Column/Overrides/IdPick") as OptionButton
	_expect(id_pick.item_count == 2, "the identity picker offers the evidence and each candidate class")
	card.set(&"_dropped", true)
	card.call(&"_render")
	_expect(not (card.get_node("Scroll/Column/Overrides") as Control).visible and facts.visible, "a dropped track keeps only its note and Close")
	card.set(&"_dropped", false)
	# A struck track says so on the stance line (L2542), which sits above the card's fold even at 200%
	# interface scale: 960 x 540 logical, where the card's scroll is at most 45% of the height.
	card.call(&"show_track", _track(413, Bp.STANCE_HOSTILE, {"struck": true, "emitting": true, "inbound": true, "envelope_m": 3000.0}), {})
	var stance := card.get_node("Scroll/Column/Stance") as Label
	_expect(stance.text.contains(tr(&"hud_tracks.card.stance_struck").format([""]).strip_edges().trim_prefix(".").strip_edges()), "the stance line says Struck colours: %s" % stance.text)
	_expect(not facts.text.contains("Struck"), "the facts do not repeat it")
	var column := card.get_node("Scroll/Column") as VBoxContainer
	var above := 0.0
	for child: Node in column.get_children():
		var c := child as Control
		if c == null or not c.visible:
			continue
		above += c.get_combined_minimum_size().y + float(column.get_theme_constant(&"separation"))
		if c == stance:
			break
	_expect(above <= 540.0 * 0.45, "the struck line is inside the first %.0f px of the card at 200%% (%.0f)" % [540.0 * 0.45, above])
	card.call(&"show_track", _track(412, Bp.STANCE_HOSTILE), {})
	_expect(not stance.text.contains("Struck"), "an unstruck track's stance line is the word alone: %s" % stance.text)


# --- plot ----------------------------------------------------------------------------------------

func _plot_checks() -> void:
	var plot := _node("Frame/View/PlotTable")
	_expect(plot.call(&"plot") is Control, "the panel hosts a BpPlotView")
	plot.call(&"set_tool", &"measure")
	_expect(plot.call(&"tool") == &"measure", "the measure tool engages")
	_expect(bool(plot.call(&"cancel_tool")) and plot.call(&"tool") == &"orders", "cancel leaves the tool for orders")
	_expect(not bool(plot.call(&"cancel_tool")), "with the orders tool there is nothing to cancel")
	plot.call(&"set_tool", &"line")
	_expect(plot.call(&"tool") == &"orders", "the mine line is refused while the hull cannot lay mines")
	_expect(bool(plot.call(&"centre_on_entry", {"track": -1, "pos": Vector2(1200, -300)})), "a log entry with a position centres the plot")
	_expect(not bool(plot.call(&"centre_on_entry", {"track": -1, "pos": null})), "an entry about nothing does nothing")
	var script: GDScript = load("res://ui/hud/plot_table.gd")
	_expect(script.call(&"_legend_tint", &"stance_hostile_air") == &"stance.hostile", "stance legend rows take their stance tint")
	_expect(script.call(&"_legend_tint", &"track_bearing_wedge") == &"text.primary", "shape legend rows are ink")
	_expect(is_equal_approx(float(script.call(&"_step_mpp", 4)), 25.0) and is_equal_approx(float(script.call(&"_step_mpp", 99)), 250.0), "the step table clamps")


# --- kill cam ------------------------------------------------------------------------------------

func _kill_cam_checks() -> void:
	var kc := _node("Frame/Overlays/KillCam")
	var panel := kc.get_node("Panel") as Control
	kc.call(&"show_clip", {"from_s": 30.0, "to_s": 10.0})
	_expect(not panel.visible, "a clip that ends before it starts is ignored")
	kc.call(&"show_clip", {"victim": 5, "killer": 9, "from_s": 10.0, "to_s": 30.0})
	_expect(panel.visible and bool(kc.call(&"is_open")), "a clip is offered")
	kc.call(&"_on_dismiss")
	_expect(not panel.visible, "Dismiss closes the offer")
