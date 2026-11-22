extends Node
## Windowed capture of the chart table in one state, for review against DESIGN.md "Interface and
## visual design". Units ui-chart, ui-chart-panels, ui-chart-cove. Run from the repository root with
## a throwaway HOME (live states start and save a real campaign):
##   HOME=/tmp/bp-chart godot --path game --resolution 1600x900 res://test/ui/chart_capture.tscn -- \
##       --state=<state> --out=<folder>
## States: live (a new campaign), live_select (a cove selected with its transit preview),
## live_advancing (a transit committed, captured on passage), busy (fixture: Act 2 with contacts,
## cut lane, cards, proposals, hunter warning), busy_cove, busy_bulletin, busy_log, away (fixture:
## SEXTANT on passage, hull lost under Ironman), empty (no campaign), busy_hc (busy in high contrast).
## Waits about 60 frames, writes <folder>/chart_<state>.png and quits; exits 1 on a failed setup.

const TABLE := preload("res://ui/chart/chart_table.tscn")
const Fixture := preload("res://test/ui/chart_fixture.gd")
const WAIT_FRAMES := 60

var _state := "live"
var _out := "user://captures"
var _frame := 0
var _table: Control
var _fixture: RefCounted
var _shot_at := WAIT_FRAMES


func _ready() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--state="):
			_state = arg.trim_prefix("--state=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	DirAccess.make_dir_recursive_absolute(_out)
	if _state.ends_with("_hc"):
		_set_palette(1)
	for scale_pct: int in [75, 150, 200]:
		if _state.ends_with("_s%d" % scale_pct):
			get_tree().root.content_scale_factor = scale_pct / 100.0


func _set_palette(value: int) -> void:
	var settings := get_node_or_null(^"/root/Settings")
	if settings != null and settings.has_method(&"set_value"):
		settings.call(&"set_value", &"access.palette", value)
	var ui := get_node_or_null(^"/root/Ui")
	if ui != null and ui.has_method(&"_apply_palette"):
		ui.call(&"_apply_palette", &"high_contrast" if value == 1 else &"dusk")


func _process(_delta: float) -> void:
	_frame += 1
	if _frame == 2:
		if not _setup():
			get_tree().quit(1)
			return
	if _state == "live_focus" and _frame >= 20 and _frame <= 44 and _frame % 4 == 0:
		# Keyboard use: arrows move the chart selection, then Right past the chart's east edge
		# hands focus to the column.
		Ui.using_mouse = false
		var key := KEY_UP if _frame < 32 else KEY_RIGHT
		for pressed: bool in [true, false]:
			var ev := InputEventKey.new()
			ev.keycode = key
			ev.physical_keycode = key
			ev.pressed = pressed
			Input.parse_input_event(ev)
	if _state == "live_focus" and _frame == 50:
		print("chart_capture: focus on ", get_viewport().gui_get_focus_owner(), " selected ", _table.get_node(^"Page/Layout/Body/ChartMap").call(&"selected"))
	if _state == "live_advancing" and _frame == 40:
		_commit_transit()
		_shot_at = _frame + 30
	if _frame == _shot_at:
		var img := get_viewport().get_texture().get_image()
		var path := _out.path_join("chart_%s.png" % _state)
		var err := img.save_png(path)
		print("chart_capture: %s -> %s (%s)" % [_state, path, error_string(err)])
		if _fixture != null:
			print("chart_capture: sent ", _fixture.get(&"sent"))
		get_tree().quit(0 if err == OK else 1)


func _setup() -> bool:
	match _state:
		"live", "live_select", "live_advancing", "live_focus":
			var campaign := get_node_or_null(^"/root/Campaign")
			if campaign == null:
				return false
			if not campaign.call(&"is_active"):
				var problems: Array = campaign.call(&"new_campaign", {"slot_name": "Chart capture", "ironman": false, "seed": 4242, "starter": "starters/fac22.bp.ron", "tutorial_done": true,
					"difficulty": {"preset": &"normal", "ai_accuracy": 1.0, "ai_reaction_s": 2.0, "adaptation_cadence": 1.0, "hull_count": 300, "economy": 1.0, "clock_days": 40}})
				if not problems.is_empty():
					printerr("chart_capture: new_campaign refused: ", problems)
					return false
			_table = TABLE.instantiate()
			add_child(_table)
			if _state == "live_select":
				var map: Control = _table.get_node(^"Page/Layout/Body/ChartMap")
				map.call(&"select", &"cove.pell", true)
			if _state == "live_focus":
				_table.get_node(^"Page/Layout/Body/ChartMap").grab_focus()
			else:
				Ui.focus(_table.call(&"default_focus"))
		"empty":
			_fixture = Fixture.new()
			_table = TABLE.instantiate()
			add_child(_table)
			_table.call(&"use_source", _fixture)
		_:
			_fixture = Fixture.new()
			var s: Dictionary = _fixture.call(&"busy_state")
			if _state == "away":
				s["chart"]["at_cove"] = false
				s["chart"]["phase"] = 2
				s["chart"]["can_save"] = 2
				s["chart"]["proposals"] = []
				s["advancing"] = true
				s["players"][0]["hull_lost"] = true
				s["quotes"]["rebuild"] = {"salvage": 620, "hours": 48, "needed": true, "allowed": false}
				s["slot"]["ironman"] = true
			if _state.begins_with("busy_cove"):
				s["chart"]["proposals"] = []
				s["players"].resize(1)
			_fixture.set(&"state", s)
			_table = TABLE.instantiate()
			add_child(_table)
			_table.call(&"use_source", _fixture)
			var tabs: TabContainer = _table.get_node(^"Page/Layout/Body/Side/Margin/Scroll/Column/Tabs")
			match _state:
				"busy_cove", "away":
					tabs.current_tab = 1
				"busy_bulletin":
					tabs.current_tab = 2
				"busy_log":
					tabs.current_tab = 3
			var map: Control = _table.get_node(^"Page/Layout/Body/ChartMap")
			if _state != "away":
				map.call(&"select", &"cove.wren", true)
				map.call(&"flash", &"region", &"region.gannet")
				map.call(&"flash", &"lane", &"depot.skerrow", &"sea.saddle")
			else:
				map.call(&"set_passage", [&"cove.pell", &"radar.crane", &"base.ollan", &"sea.saltmere", &"harbour.mareby", &"cove.wren"], 0.4)
	return true


func _commit_transit() -> void:
	var campaign := get_node_or_null(^"/root/Campaign")
	campaign.call(&"set_advance_speed", 3.0)
	var map: Control = _table.get_node(^"Page/Layout/Body/ChartMap")
	map.call(&"select", &"cove.pell", true)
	var transit: Button = _table.get_node(^"Page/Layout/Body/Side/Margin/Scroll/Column/Place/Actions/Transit")
	transit.emit_signal(&"pressed")
	print("chart_capture: advancing ", campaign.call(&"is_advancing"))
