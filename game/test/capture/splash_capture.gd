extends Node
## Windowed capture of a splash (unit gd-editor-splash): opens a sandbox dock on a starter, splashes it, shows
## res://world/world.tscn, and saves the basin before and after a live parts edit (the hull is replaced in the
## basin and must still be drawn), then ends the splash and prints the report card:
##   godot --path game --resolution 1600x900 res://test/capture/splash_capture.tscn -- --out=/abs/dir
## Headless, and when instanced by check_project, it does nothing.

var _out := "user://captures"
var _world: Node3D
var _frame := 0
var _sortie := -1
var _active := false
var _card: Dictionary = {}


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	DirAccess.make_dir_recursive_absolute(_out)
	_world = (load("res://world/world.tscn") as PackedScene).instantiate() as Node3D
	add_child(_world)
	Editor.splash_ended.connect(func(card: Dictionary) -> void: _card = card)
	Editor.open(Bp.DOCK_SANDBOX, {"blueprint": "starters/corvette40.bp.ron"})
	_active = true


func _shot(name: String) -> void:
	var path := _out.path_join("splash_%s.png" % name)
	get_viewport().get_texture().get_image().save_png(path)
	print("splash_capture: saved ", path)


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame == 90:
		print("splash_capture: splash ", Editor.splash({"water": 3, "target": Bp.SPLASH_TARGET_PATROL_BOAT}))
	if _frame > 3000:
		push_error("splash_capture: timed out")
		_end(1)
		return
	if _frame < 90:
		return
	if _sortie < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE:
			_sortie = _frame
			print("splash_capture: load ", Editor.splash_stats())
		return
	var t := _frame - _sortie
	if t == 120:
		_shot("basin")
		Editor.select_parts(PackedInt32Array([0]), false)
		print("splash_capture: removed ", Editor.delete_selection(), " apply ", Editor.splash_apply_edits())
	if t == 300:
		_shot("after_edit")
		Editor.splash_end()
	if t > 300 and not _card.is_empty():
		print("splash_capture: card ", _card)
		_end(0)


func _end(code: int) -> void:
	_active = false
	Session.leave(false)
	Editor.close()
	get_tree().quit(code)
