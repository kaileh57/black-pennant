extends Node
## Windowed capture of the first playable sortie (units gd-session, gd-host, gd-link-channel): starts a
## skirmish through the Session autoload with the 22 m starter, shows res://world/world.tscn, orders a course
## once the sortie runs, and saves PNGs of the world at two moments:
##   godot --path game --resolution 1600x900 res://test/capture/sortie_capture.tscn -- --out=/abs/dir
## It proves what the renderer gets from the session: a world, the own hull's design built for drawing, and
## a ship that answers the helm. Headless, and when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
## Sortie frames after which each shot is taken, and the run's end.
const SHOTS: Array[int] = [120, 480]
const LIMIT := 2400

var _out := "user://captures"
var _world: Node3D
var _check: BpSessionCheck
var _frame := 0
var _sortie_frame := -1
var _shot := 0
var _active := false


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	DirAccess.make_dir_recursive_absolute(_out)
	_check = BpSessionCheck.new()
	_world = (load("res://world/world.tscn") as PackedScene).instantiate() as Node3D
	add_child(_world)
	var problems := Session.start_skirmish({
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 2, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
	})
	if not problems.is_empty():
		push_error("sortie_capture: start_skirmish refused: " + str(problems))
		get_tree().quit(1)
		return
	_active = true


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame > LIMIT:
		push_error("sortie_capture: the sortie did not run long enough")
		_end(1)
		return
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE:
			_sortie_frame = _frame
		elif Session.state() == Bp.SESSION_FAILED:
			push_error("sortie_capture: the session failed")
			_end(1)
		return
	var since := _frame - _sortie_frame
	if since == 30:
		var f := _check.facts()
		var heading := deg_to_rad(float(f.get("heading_deg", 0.0)) + 50.0)
		_check.set_course(Vector2(f.get("pos", Vector2.ZERO)) + Vector2(sin(heading), -cos(heading)) * 3000.0, 25.0)
	if _shot < SHOTS.size() and since == SHOTS[_shot]:
		var image := get_viewport().get_texture().get_image()
		var path := _out.path_join("sortie_%d.png" % _shot)
		image.save_png(path)
		print("sortie_capture: saved ", path, " ", _check.facts())
		_shot += 1
		if _shot == SHOTS.size():
			_end(0)


func _end(code: int) -> void:
	_active = false
	Session.leave(false)
	get_tree().quit(code)
