extends Node
## Windowed play-through of the sortie HUD from the main menu, by real input only: clicks on the
## menu's Skirmish and the setup's Start, then keys and clicks in the sortie (Tab, the next-track
## key, a click on a track symbol, time compression, the log key, a station button and its close
## button, the helm key with W and A), saving a PNG at each step and a layout audit line:
##   godot --path game --resolution 1920x1080 res://test/capture/hud_play_capture.tscn -- \
##     --out=/abs/dir [--scale=1] [--small=1280x720]
## With --small the window is resized partway through and the bridge is shot again.
## Headless, or when instanced by check_project, it does nothing.

const LIMIT := 60 * 600

var _out := "/tmp"
var _scale := 1.0
var _small := Vector2i.ZERO
var _frame := 0
var _active := false
var _step := 0
var _wait := 0
var _failures := 0
var _walk: PackedStringArray = []


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"out": _out = kv[1]
			"scale": _scale = float(kv[1])
			"small":
				var wh := kv[1].split("x")
				_small = Vector2i(int(wh[0]), int(wh[1]))
	DirAccess.make_dir_recursive_absolute(_out)
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_scale", _scale)
	Screens.start_routing()
	Screens.change(&"main_menu")
	_active = true


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame > LIMIT:
		_fail("ran out of frames at step %d" % _step)
		_end()
		return
	if _wait > 0:
		_wait -= 1
		return
	_run()


func _run() -> void:
	match _step:
		0:
			if not _screen_named("MainMenu"):
				return
			_shot("menu")
			_click_control(_find(Screens.top(), "Skirmish"))
			_wait = 30
		1:
			if not _screen_named("SkirmishSetup"):
				return
			_shot("setup")
			_click_control(_find(Screens.top(), "Start"))
			_wait = 30
		2:
			if Session.state() != Bp.SESSION_IN_SORTIE or Screens.is_loading() or _hud() == null:
				if Session.state() == Bp.SESSION_FAILED:
					_fail("the session failed")
					_end()
				return
			_wait = 240
		3:
			_shot("sortie_start")
			_tap(KEY_PERIOD)
			_wait = 30
		4:
			_shot("next_track")
			_tap(KEY_TAB)
			_wait = 40
		5:
			_shot("plot")
			_tap(KEY_TAB)
			_wait = 20
		6:
			# Designate the first hostile by clicking its symbol on the bridge overlay.
			var at := _first_symbol()
			if at.x >= 0.0:
				_click_canvas(at, MOUSE_BUTTON_LEFT)
			_wait = 30
		7:
			_shot("designated")
			_tap(KEY_EQUAL)
			_tap(KEY_EQUAL)
			_wait = 60 * 12
		8:
			_shot("after_time")
			_tap(KEY_L)
			_wait = 30
		9:
			_shot("log")
			_tap(KEY_L)
			_wait = 10
		10:
			_click_control(_hud().get_node(^"Frame/Docks/RightDock/Column/Stations/Grid/Damage"))
			_wait = 30
		11:
			_shot("damage")
			_click_control(_hud().get_node(^"Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Header/Close"))
			_wait = 20
		12:
			_tap(KEY_MINUS)
			_tap(KEY_MINUS)
			_tap(KEY_H)
			_wait = 40
		13:
			_key(KEY_A, true)
			_wait = 60
		14:
			_shot("helm_held")
			_key(KEY_A, false)
			_tap(KEY_H)
			_wait = 30
		15:
			if _small != Vector2i.ZERO:
				DisplayServer.window_set_size(_small)
			_wait = 60
		16:
			_shot("final")
			# The keyboard walk: Shift+Tab into the docks, then Right along the panels.
			var e := InputEventKey.new()
			e.physical_keycode = KEY_TAB
			e.keycode = KEY_TAB
			e.shift_pressed = true
			e.pressed = true
			Input.parse_input_event(e)
			var r := e.duplicate() as InputEventKey
			r.pressed = false
			Input.parse_input_event(r)
			_wait = 5
		17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36:
			var f := get_viewport().gui_get_focus_owner()
			var path := String(f.get_path()) if f != null else "none"
			_walk.append(path)
			print("hud_play: focus ", path)
			_tap(KEY_RIGHT)
			_wait = 4
		37:
			for want: String in ["TrackList", "HelmCluster", "Objectives", "Stations"]:
				var hit := false
				for p: String in _walk:
					hit = hit or p.contains(want)
				if not hit:
					_fail("Right never reached " + want)
			_shot("focus_walk")
			print("hud_play: done, failures ", _failures)
			_end()
			return
	_step += 1


func _screen_named(stem: String) -> bool:
	var top: Node = Screens.top()
	return top != null and String(top.name) == stem


func _hud() -> Node:
	var top: Node = Screens.top()
	return top if top != null and top.has_method(&"toggle_view") else null


func _find(root: Node, node_name: String) -> Control:
	if root == null:
		return null
	var found := root.find_child(node_name, true, false)
	return found as Control


func _first_symbol() -> Vector2:
	var hud := _hud()
	if hud == null:
		return Vector2(-1, -1)
	var overlay := hud.get_node(^"Frame/View/BridgeOverlay") as Control
	var laid: Array = overlay.call(&"layout_entries")
	for e: Dictionary in laid[0]:
		if bool(e["on"]):
			return overlay.get_global_transform() * Vector2(e["pos"])
	return Vector2(-1, -1)


func _click_control(c: Control) -> void:
	if c == null or not c.is_visible_in_tree():
		_fail("no control to click at step %d" % _step)
		return
	_click_canvas(c.get_global_rect().get_center(), MOUSE_BUTTON_LEFT)


func _click_canvas(at: Vector2, button: MouseButton) -> void:
	var window := get_viewport().get_final_transform() * at
	var move := InputEventMouseMotion.new()
	move.position = window
	move.global_position = window
	Input.parse_input_event(move)
	for p: bool in [true, false]:
		var e := InputEventMouseButton.new()
		e.button_index = button
		e.pressed = p
		e.position = window
		e.global_position = window
		if p:
			e.button_mask = MOUSE_BUTTON_MASK_LEFT if button == MOUSE_BUTTON_LEFT else MOUSE_BUTTON_MASK_RIGHT
		Input.parse_input_event(e)


func _key(code: Key, pressed: bool) -> void:
	var e := InputEventKey.new()
	e.physical_keycode = code
	e.keycode = code
	e.pressed = pressed
	Input.parse_input_event(e)


func _tap(code: Key) -> void:
	_key(code, true)
	_key(code, false)


func _shot(name: String) -> void:
	var path := _out.path_join("%02d_%s.png" % [_step, name])
	get_viewport().get_texture().get_image().save_png(path)
	var hud := _hud()
	var extra := ""
	if hud != null:
		extra = " view %d station %s tracks %d selected %d designated %d" % [int(hud.call(&"view")), str(hud.call(&"station")), Hud.tracks().size(), Hud.selected_track(), Hud.designated_track()]
	print("hud_play: saved ", path, extra)


func _fail(text: String) -> void:
	_failures += 1
	push_error("hud_play: " + text)


func _end() -> void:
	_active = false
	get_tree().quit(1 if _failures > 0 else 0)
