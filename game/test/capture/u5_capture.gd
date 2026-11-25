extends Node
## Windowed captures of the briefing, after-action, sortie summary and special-operations bar with
## hand-built data (units ui-briefing, ui-after-action, ui-hud-ops). One screen and state per run:
##   godot --path game --resolution 1600x900 res://test/capture/u5_capture.tscn -- \
##     --screen=briefing --state=populated --palette=dusk --scale=1 --out=/abs/dir
## Screens: briefing, after_action, sortie_end, actions_bar. States: populated, empty, and per screen
## coop, stale (briefing), defeat (after_action, sortie_end), pvp (sortie_end), prize, pick (actions_bar).
## Waits 60 frames, saves <screen>_<state>_<palette>_<scale>x.png, quits. Headless, or when instanced by
## check_project, it does nothing.

const Samples := preload("res://test/capture/u5_samples.gd")

var _screen := "briefing"
var _state := "populated"
var _palette := "dusk"
var _scale := 1.0
var _out := "user://captures"
var _frame := 0
var _active := false
var _node: Node


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--screen="):
			_screen = arg.trim_prefix("--screen=")
		elif arg.begins_with("--state="):
			_state = arg.trim_prefix("--state=")
		elif arg.begins_with("--palette="):
			_palette = arg.trim_prefix("--palette=")
		elif arg.begins_with("--scale="):
			_scale = float(arg.trim_prefix("--scale="))
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	DirAccess.make_dir_recursive_absolute(_out)
	Ui.call(&"_apply_palette", StringName(_palette))
	Ui.call(&"_apply_scale", _scale)
	_build.call_deferred()
	_active = true


func _build() -> void:
	match _screen:
		"briefing":
			_node = _add(&"res://ui/screens/briefing.tscn")
			match _state:
				"empty":
					pass
				"stale":
					_node.call(&"show_briefing", Samples.briefing(), Samples.fit(4))
				"idle":
					_node.call(&"show_briefing", Samples.briefing(), {"state": 0})
				"coop":
					_node.call(&"show_briefing", Samples.briefing(), Samples.fit(2))
					_node.call(&"show_coop", Samples.players(), Samples.accept_proposal(), true)
				_:
					_node.call(&"show_briefing", Samples.briefing(), Samples.fit(2))
		"after_action":
			_node = _add(&"res://ui/screens/after_action.tscn")
			match _state:
				"empty":
					pass
				"defeat":
					_node.call(&"show_report", Samples.after_action(false), Samples.players(), Samples.regions())
				_:
					_node.call(&"show_report", Samples.after_action(true), Samples.players(), Samples.regions())
		"sortie_end":
			var world := ColorRect.new()
			world.color = Ui.colour(&"surface.base")
			world.set_anchors_preset(Control.PRESET_FULL_RECT)
			add_child(world)
			_node = _add(&"res://ui/screens/sortie_end.tscn")
			match _state:
				"empty":
					pass
				"pvp":
					_node.call(&"show_summary", Samples.summary(true), 4, Samples.objectives())
				"defeat":
					_node.call(&"show_summary", Samples.summary(false), 2, Samples.objectives())
				_:
					_node.call(&"show_summary", Samples.summary(true), 2, Samples.objectives())
		"actions_bar":
			var world := ColorRect.new()
			world.color = Ui.colour(&"surface.inset")
			world.set_anchors_preset(Control.PRESET_FULL_RECT)
			add_child(world)
			_node = _add(&"res://ui/hud/actions_bar.tscn")
			(_node as Control).position = Vector2(48.0, 48.0)
			match _state:
				"empty":
					_node.call(&"show_ops", [], [], [], 0)
				"prize":
					_node.set(&"_prize_vessel", {3: 9001})
					_node.call(&"show_ops", Samples.available(), Samples.actions_done(), Samples.offboard(), 12)
				"pick":
					_node.call(&"show_ops", Samples.available(), Samples.actions(), Samples.offboard(), 12)
					_node.call(&"begin_pick", 7, 2)
				_:
					_node.call(&"show_ops", Samples.available(), Samples.actions(), Samples.offboard(), -1)


func _add(path: StringName) -> Node:
	var n := (load(String(path)) as PackedScene).instantiate()
	add_child(n)
	return n


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame == 60:
		var img := get_viewport().get_texture().get_image()
		var file := "%s/%s_%s_%s_%sx.png" % [_out, _screen, _state, _palette, str(_scale)]
		var err := img.save_png(file)
		if err != OK:
			push_error("u5_capture: cannot save " + file)
		print("u5_capture: saved ", file)
		get_tree().quit(0 if err == OK else 1)
