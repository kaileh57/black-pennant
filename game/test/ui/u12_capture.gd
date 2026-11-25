extends Node
## Windowed capture of the dry dock's control screens (units ui-editor-control and ui-editor-graph), for
## review against DESIGN.md "Interface and visual design". Run with a throwaway HOME (saves write user://):
##   HOME=/tmp/bp-u12 godot --path game --resolution 1600x900 res://test/ui/u12_capture.tscn -- \
##       --state=<state> --out=<folder> [--bp=starters/fac22.bp.ron]
## States: officers, officers_tuning, officers_mount, officers_folds, rules, rules_builder, rules_full,
## graph, graph_all, graph_selected, graph_library, graph_forked, scripts, closed; any state with _hc (high
## contrast) or _s200 (UI scale 200%) appended. The 3D stage gets the dock capture's dusk light. Writes
## <folder>/u12_<state>.png and quits; exits 1 on a failed setup. Headless, and when instanced by
## check_project, it does nothing.

var _state := "officers"
var _base := "officers"
var _out := "user://captures"
var _bp := "starters/fac22.bp.ron"
var _frame := 0
var _dock: Control
var _shot_at := -1
var _step_at := -1


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--state="):
			_state = arg.trim_prefix("--state=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--bp="):
			_bp = arg.trim_prefix("--bp=")
	_base = _state.replace("_hc", "").replace("_s200", "")
	DirAccess.make_dir_recursive_absolute(_out)
	var settings := get_node_or_null(^"/root/Settings")
	if settings != null and settings.has_method(&"reload"):
		settings.call(&"reload")
	Ui.refresh()
	if _state.contains("_hc"):
		Settings.set_value(&"access.palette", 1)
		Ui.call(&"_apply_palette", &"high_contrast")
	if _state.contains("_s200"):
		get_tree().root.content_scale_factor = 2.0


func _process(_delta: float) -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	_frame += 1
	if _frame == 2 and not _setup():
		get_tree().quit(1)
		return
	if _frame > 2400:
		push_error("u12_capture: timed out")
		get_tree().quit(1)
		return
	if _frame == 150:
		_apply_state()
		_step_at = _frame + 30
	if _frame == _step_at:
		_second_step()
		_shot_at = _frame + 60
	if _frame == _shot_at:
		var path := _out.path_join("u12_%s.png" % _state)
		get_viewport().get_texture().get_image().save_png(path)
		print("u12_capture: saved ", path)
		Editor.close()
		get_tree().quit(0)


func _setup() -> bool:
	if _base == "closed":
		Editor.close()
		Screens.change(&"dry_dock")
	else:
		var issues: Array = Screens.open_dry_dock(Bp.DOCK_SANDBOX, {"blueprint": _bp})
		for i: Dictionary in issues:
			if bool(i.get("fatal", false)):
				push_error("u12_capture: open refused " + str(issues))
				return false
	_dock = Screens.top() as Control
	if _dock == null:
		push_error("u12_capture: no dock screen")
		return false
	var helper: Node = (load("res://test/ui/dock_ui_capture.gd") as GDScript).new()
	helper.call(&"_light", _dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport") as SubViewport)
	helper.free()
	return true


func _node(path: String) -> Node:
	return _dock.get_node_or_null(NodePath(path))


func _officers() -> void:
	(_node("Scroll/Frame/Body/LeftDock/Tabs") as TabContainer).current_tab = 3


func _panel() -> Node:
	return _node("Scroll/Frame/Body/LeftDock/Tabs/Officers/Column/Control")


func _rules() -> Node:
	return _node("Scroll/Frame/Body/LeftDock/Tabs/Officers/Column/Rules")


func _graph() -> Node:
	return _node("Scroll/Frame/Body/Centre/Stage/Graph")


func _scroll_to(c: Control) -> void:
	var scroll := _node("Scroll/Frame/Body/LeftDock/Tabs/Officers") as ScrollContainer
	scroll.scroll_vertical = int(c.global_position.y - scroll.global_position.y + scroll.scroll_vertical - 8.0)


func _add_rules(n: int) -> void:
	var kinds: Array = [&"own_speed_kn", &"fuel_fraction", &"hull_health", &"inbound_weapons"]
	for i in n:
		var r: Dictionary = {"enabled": i != 1, "when": {"op": &"compare", "lhs": {"kind": kinds[i % kinds.size()]}, "cmp": Bp.CMP_LT,
			"rhs": {"kind": &"const", "value": 12.0 if i % 4 == 0 else 0.3}}, "sink": {"kind": &"helm_speed", "part": -1}, "value": {"type": &"scalar", "value": 10.0 + i}}
		if i == 2:
			var sinks: Array = Editor.rule_vocabulary()["sinks"]
			var sink: Dictionary = sinks[0]["sink"]
			for s: Dictionary in sinks:
				if s["value_type"] == &"nearest_contact":
					sink = s["sink"]
					break
			r = {"enabled": true, "when": {"op": &"all", "items": [
				{"op": &"compare", "lhs": {"kind": &"nearest_contact_range_m", "filter": {"min_stance": Bp.STANCE_HOSTILE, "domain": -1, "min_grade": 0}}, "cmp": Bp.CMP_LT, "rhs": {"kind": &"const", "value": 4000.0}},
				{"op": &"compare", "lhs": {"kind": &"hull_health"}, "cmp": Bp.CMP_GT, "rhs": {"kind": &"const", "value": 0.5}}]},
				"sink": sink, "value": {"type": &"nearest_contact", "value": {"min_stance": Bp.STANCE_HOSTILE, "domain": -1, "min_grade": 0}}}
		var issues: Array = Editor.add_rule(-1, r)
		if not issues.is_empty():
			print("u12_capture: add_rule ", issues)


func _apply_state() -> void:
	match _base:
		"officers", "officers_tuning", "officers_folds", "officers_mount":
			_officers()
			Editor.set_slider("gunnery.aggression", 0.8)
			Editor.set_power_priority(17, 2)
		"rules", "rules_builder", "rules_full":
			_officers()
			_add_rules(32 if _base == "rules_full" else 3)
		"graph", "graph_all", "graph_selected", "graph_library", "graph_forked":
			_dock.call(&"_set_mode", 1)
			if _base == "graph_forked":
				Editor.graph_replace_stage(Bp.STAGE_THREAT_RANKING)
		"scripts":
			_dock.call(&"_set_mode", 2)
		"scripts_full":
			var a := Editor.script_add("Close-in guard")
			Editor.script_set_source(a, "--@in range: scalar\n--@in bearing: angle\n--@out fire: bool\n\n-- Fire the close-in gun only when the threat is inside 2 km.\nfunction tick(i)\n  local near = i.range < 2000\n  return { fire = near }\nend\n")
			var b := Editor.script_add("Zig-zag")
			Editor.script_set_source(b, "--@in time: scalar\n--@out heading: angle\nfunction tick(i\n  return { heading = (i.time % 60) * 3 }\nend\n")
			Editor.script_set_enabled(b, false)
			_dock.call(&"_set_mode", 2)
	print("u12_capture: state ", _base, " rules ", Editor.rules().size(), " layers ", Editor.layers())


func _second_step() -> void:
	match _base:
		"officers_tuning":
			_scroll_to(_panel().find_children("*", "HSlider", true, false)[0] as Control)
		"officers_mount":
			var pick := _panel().find_children("*", "OptionButton", true, false)[0] as OptionButton
			if pick.item_count > 1:
				pick.select(1)
				pick.item_selected.emit(1)
		"officers_folds":
			for f: Node in _panel().find_children("*", "FoldableContainer", true, false):
				(f as FoldableContainer).folded = false
			await get_tree().process_frame
			var folds := _panel().find_children("*", "FoldableContainer", true, false)
			_scroll_to(folds[folds.size() - 2] as Control)
		"rules", "rules_full":
			_scroll_to(_rules() as Control)
		"rules_builder":
			_rules().call(&"_on_edit", 2)
		"graph_all":
			_graph().call(&"set_officer", -1)
		"graph_selected":
			_graph().call(&"focus_node", 3211264)
		"graph_library":
			_graph().call(&"open_library")
		"graph_forked":
			_graph().call(&"set_officer", Bp.OFFICER_GUNNERY)
