extends Control
## Windowed end-to-end check of the plot table in a real sortie (unit gd-plot): starts a skirmish through
## the Session autoload, shows a BpPlotView over it, saves PNGs once tracks appear, clicks the first track
## and the open sea through the input system and checks the signals that come back:
##   godot --path game --resolution 1600x900 res://test/capture/plot_sortie_capture.tscn -- --out=/abs/dir
## Exits 1 on any failure. Headless, and when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 3600
const SHOT_AT: Array[int] = [240, 900]
## After the first shot: centre on a contact and click it, right-click the sea, then (frames counted from
## the first shot) hover the contact, snap the gamepad cursor and designate with a joypad button, draw a
## line with the line tool, and turn every layer on.
const T_CENTRE := 10
const T_CLICK := 14
const T_SEA := 20
const T_HOVER := 30
const T_PAD := 36
const T_LINE_A := 44
const T_LINE_B := 48
const T_CHECK := 56

var _out := "user://captures"
var _plot: BpPlotView
var _check: BpSessionCheck
var _frame := 0
var _sortie_frame := -1
var _shot := 0
var _active := false
var _failures := 0
var _clicked: Array = []
var _sea: Array = []
var _scales: Array = []
var _hovers: Array = []
var _lines: Array = []
var _target := -1
var _target_screen := Vector2.ZERO


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	DirAccess.make_dir_recursive_absolute(_out)
	_check = BpSessionCheck.new()
	_plot = BpPlotView.new()
	_plot.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(_plot)
	_plot.track_clicked.connect(func(t: int, b: int) -> void: _clicked.append([t, b]))
	_plot.sea_clicked.connect(func(m: Vector2, b: int) -> void: _sea.append([m, b]))
	_plot.scale_changed.connect(func(s: int, mpp: float) -> void: _scales.append([s, mpp]))
	_plot.hover_changed.connect(func(t: int) -> void: _hovers.append(t))
	_plot.line_drawn.connect(func(a: Vector2, b: Vector2) -> void: _lines.append([a, b]))
	var problems := Session.start_skirmish({
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 2, "difficulty": {"preset": "normal"},
		"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
	})
	if not problems.is_empty():
		push_error("plot_sortie_capture: start_skirmish refused: " + str(problems))
		get_tree().quit(1)
		return
	_active = true


func _click(at: Vector2, button: MouseButton) -> void:
	for pressed: bool in [true, false]:
		var e := InputEventMouseButton.new()
		e.button_index = button
		e.pressed = pressed
		e.position = at
		e.global_position = at
		Input.parse_input_event(e)


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame > LIMIT:
		_fail("the sortie did not run long enough")
		_end()
		return
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE:
			_sortie_frame = _frame
			_plot.set_scale_step(5)
		elif Session.state() == Bp.SESSION_FAILED:
			_fail("the session failed")
			_end()
		return
	var since := _frame - _sortie_frame
	if _shot < SHOT_AT.size() and since == SHOT_AT[_shot]:
		var image := get_viewport().get_texture().get_image()
		var path := _out.path_join("plot_sortie_%d.png" % _shot)
		image.save_png(path)
		print("plot_sortie_capture: saved ", path, " legend=", _plot.legend().size(), " hidden=", _plot.hidden_labels_at(Vector2(800, 450)))
		_shot += 1
	var k := since - SHOT_AT[0]
	if k == T_CENTRE:
		var tracks := _contacts()
		print("plot_sortie_capture: contacts in the picture: ", tracks.size())
		if tracks.is_empty():
			_fail("no contacts after %d frames" % since)
		else:
			_target = tracks[0]["id"]
			if not _plot.centre_on_track(_target):
				_fail("centre_on_track refused a live track")
	if k == T_CLICK and _target > 0:
		_target_screen = _to_window(_plot.from_map(Hud.track(_target)["pos"]))
		_click(_target_screen, MOUSE_BUTTON_LEFT)
	if k == T_SEA:
		_click(Vector2(DisplayServer.window_get_size()) * Vector2(0.1, 0.85), MOUSE_BUTTON_RIGHT)
		_plot.zoom_step(-1, Vector2(800, 450))
	if k == T_HOVER and _target > 0:
		var m := InputEventMouseMotion.new()
		m.position = _to_window(_plot.from_map(Hud.track(_target)["pos"]))
		m.global_position = m.position
		Input.parse_input_event(m)
	if k == T_PAD and _target > 0:
		# Park the cursor 18 px off the contact: inside the 24 px snap, outside the 14 px click radius.
		var at := _plot.from_map(Hud.track(_target)["pos"])
		_plot.move_plot_cursor(at + Vector2(18, 0) - _plot.size * 0.5)
		var pad := InputEventJoypadButton.new()
		pad.button_index = JOY_BUTTON_A
		pad.pressed = true
		Input.parse_input_event(pad)
		var up := InputEventJoypadButton.new()
		up.button_index = JOY_BUTTON_A
		up.pressed = false
		Input.parse_input_event(up)
		print("plot_sortie_capture: cursor ", _plot.plot_cursor())
	if k == T_LINE_A:
		_plot.set_tool(&"line")
		_click(Vector2(DisplayServer.window_get_size()) * Vector2(0.3, 0.3), MOUSE_BUTTON_LEFT)
	if k == T_LINE_B:
		_click(Vector2(DisplayServer.window_get_size()) * Vector2(0.6, 0.4), MOUSE_BUTTON_LEFT)
	if k == T_CHECK:
		print("plot_sortie_capture: track_clicked ", _clicked, " sea_clicked ", _sea, " scale_changed ", _scales, " hover ", _hovers, " lines ", _lines)
		if not _clicked.any(func(e: Array) -> bool: return int(e[0]) == _target and int(e[1]) == MOUSE_BUTTON_LEFT):
			_fail("clicking a contact did not report track_clicked with the left button")
		if _clicked.filter(func(e: Array) -> bool: return int(e[0]) == _target).size() < 2:
			_fail("the joypad designate at the snapped cursor did not report the contact")
		if not _sea.any(func(e: Array) -> bool: return int(e[1]) == MOUSE_BUTTON_RIGHT):
			_fail("right-clicking the sea did not report sea_clicked with the right button")
		if _scales.size() < 2:
			_fail("scale changes were not reported")
		if not _hovers.has(_target):
			_fail("hovering the contact did not report hover_changed")
		if _lines.size() != 1:
			_fail("two clicks with the line tool did not report one line_drawn")
		_plot.set_tool(&"orders")
		_plot.set_layers(Bp.PLOT_TRACKS | Bp.PLOT_UNCERTAINTY | Bp.PLOT_ENVELOPES | Bp.PLOT_EMITTERS | Bp.PLOT_ROUTE | Bp.PLOT_RINGS | Bp.PLOT_GRATICULE | Bp.PLOT_HISTORY | Bp.PLOT_LAND | Bp.PLOT_OBJECTIVES | Bp.PLOT_MINES | Bp.PLOT_ALLIES | Bp.PLOT_LABELS)
		_plot.set_scale_step(5)
		_plot.set_follow_own(true)
	if _shot >= SHOT_AT.size():
		_end()


func _to_window(local: Vector2) -> Vector2:
	return get_viewport().get_final_transform() * (_plot.get_global_transform_with_canvas() * local)


## Tracks other than the viewer's own hull, farthest first.
func _contacts() -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for t: Dictionary in Hud.tracks():
		if float(t["range_m"]) > 50.0 and not bool(t["last_known"]):
			out.append(t)
	out.sort_custom(func(a: Dictionary, b: Dictionary) -> bool: return float(a["range_m"]) > float(b["range_m"]))
	return out


func _fail(text: String) -> void:
	push_error("plot_sortie_capture: " + text)
	_failures += 1


func _end() -> void:
	_active = false
	Session.leave(false)
	get_tree().quit(1 if _failures > 0 else 0)
