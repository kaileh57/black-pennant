extends Node
## Windowed captures and input checks of the sortie HUD (units ui-hud-shell, ui-hud-tracks,
## ui-hud-plot). Runs the real routing: Screens builds the world and the HUD for a live skirmish or
## a scripted BpTestFeed picture, then this script shoots the screen or drives input:
##   godot --path game --resolution 1600x900 res://test/capture/u6_capture.tscn -- \
##     --mode=skirmish|scripted --script=reference_battle --view=bridge|plot --select=1 \
##     --station=log --pause=1 --palette=dusk --scale=1 --frames=240 --checks=1 --out=/abs/dir
## Saves <name>.png (name from --name, default from the options). With --checks=1 it presses keys,
## clicks and gamepad buttons through Input.parse_input_event and exits 1 if an order did not land.
## Headless, or when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 6000

var _mode := "skirmish"
var _script := "reference_battle"
var _view := "bridge"
var _select := false
var _station := ""
var _pause := false
var _killcam := false
var _hover := false
var _palette := "dusk"
var _scale := 1.0
var _frames := 240
var _checks := false
var _clicks := false
var _name := ""
var _out := "/tmp"
var _feed: Node = null
var _frame := 0
var _sortie_frame := -1
var _active := false
var _failures := 0
var _step := 0
## Right presses spent walking out of the track card (check 18).
var _card_rights := 0
var _wait := 0
var _mark: Dictionary = {}


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		if kv.size() != 2:
			continue
		match kv[0]:
			"mode": _mode = kv[1]
			"script": _script = kv[1]
			"view": _view = kv[1]
			"select": _select = kv[1] == "1"
			"station": _station = kv[1]
			"pause": _pause = kv[1] == "1"
			"killcam": _killcam = kv[1] == "1"
			"hover": _hover = kv[1] == "1"
			"palette": _palette = kv[1]
			"scale": _scale = float(kv[1])
			"frames": _frames = int(kv[1])
			"checks": _checks = kv[1] == "1"
			"clicks": _clicks = kv[1] == "1"
			"name": _name = kv[1]
			"out": _out = kv[1]
	DirAccess.make_dir_recursive_absolute(_out)
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_palette", StringName(_palette))
	Ui.call(&"_apply_scale", _scale)
	Screens.start_routing()
	if _mode == "scripted":
		_feed = ClassDB.instantiate(&"BpTestFeed")
		add_child(_feed)
		if not bool(_feed.call(&"start", _script)):
			_fail("the test feed refused %s" % _script)
			get_tree().quit(1)
			return
		_feed.call(&"advance", 12)
	else:
		var problems := Session.start_skirmish({
			"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 3, "difficulty": {"preset": "normal"},
			"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": STARTER,
		})
		if not problems.is_empty():
			_fail("start_skirmish refused: " + str(problems))
			get_tree().quit(1)
			return
	_active = true


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _feed != null:
		_feed.call(&"advance", 1)
	if _frame > LIMIT:
		_fail("ran out of frames at step %d" % _step)
		_end()
		return
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading():
			_sortie_frame = _frame
		elif Session.state() == Bp.SESSION_FAILED:
			_fail("the session failed")
			_end()
		return
	var since := _frame - _sortie_frame
	var hud := _hud()
	if hud == null:
		if since > 120:
			_fail("no HUD on the stack")
			_end()
		return
	if since == 30 and _mode == "skirmish":
		_course_ahead()
	if since == _frames - 40:
		if _select:
			Hud.select_step(1)
		if _view == "plot":
			hud.call(&"set_view", Bp.VIEW_PLOT)
		if not _station.is_empty():
			hud.call(&"open_station", StringName(_station))
		if _pause:
			Session.set_paused(true)
		if _killcam:
			# The offer as it looks once a clip is ready (the hull itself is not sunk for the shot).
			hud.get_node(^"Frame/Overlays/KillCam").call(&"show_clip", {"victim": 1, "killer": -1, "from_s": 0.0, "to_s": 20.0})
	if since == _frames - 10 and _hover:
		_hover_hidden(hud)
	if since == _frames:
		_shot()
		if not _checks and not _clicks:
			_end()
		return
	if _checks and since > _frames:
		_run_checks(hud)
	elif _clicks and since > _frames:
		_run_clicks(hud)


## Point the mouse where the plot hides labels (the hover list should show them).
func _hover_hidden(hud: Node) -> void:
	var plot: Control = hud.call(&"plot_view")
	if plot == null or not plot.is_visible_in_tree():
		return
	for t: Dictionary in Hud.tracks():
		var local: Vector2 = plot.call(&"from_map", t["pos"])
		if not Rect2(Vector2.ZERO, plot.size).has_point(local):
			continue
		var hidden: PackedInt32Array = plot.call(&"hidden_labels_at", local)
		if hidden.size() >= 2:
			var m := InputEventMouseMotion.new()
			var at := get_viewport().get_final_transform() * (plot.get_global_transform_with_canvas() * local)
			m.position = at
			m.global_position = at
			m.relative = Vector2(8, 8)
			Input.parse_input_event(m)
			print("u6_capture: hovering ", local, " over hidden ", hidden)
			return
	print("u6_capture: no hidden labels to hover")


func _hud() -> Node:
	var top: Node = Screens.top()
	if top != null and top.has_method(&"toggle_view"):
		return top
	return null


func _course_ahead() -> void:
	var m: Dictionary = Hud.motion()
	if not m.has("pos"):
		return
	var h := deg_to_rad(float(m.get("heading_deg", 0.0)) + 30.0)
	Orders.set_course(PackedVector2Array([Vector2(m["pos"]) + Vector2(sin(h), -cos(h)) * 4000.0]), 20.0, false)


func _shot() -> void:
	var name := _name
	if name.is_empty():
		name = "hud_%s_%s_%s%s%s_%s_%sx" % [_mode, _view, "sel_" if _select else "", (_station + "_") if not _station.is_empty() else "", ("paused_" if _pause else "") + ("killcam_" if _killcam else "") + ("hover_" if _hover else ""), _palette, str(_scale)]
	var path := _out.path_join(name + ".png")
	get_viewport().get_texture().get_image().save_png(path)
	print("u6_capture: saved ", path, " tracks=", Hud.tracks().size(), " focus=", get_viewport().gui_get_focus_owner())


# --- input checks --------------------------------------------------------------------------------

func _key(code: Key, pressed: bool, shift: bool = false) -> void:
	var e := InputEventKey.new()
	e.physical_keycode = code
	e.keycode = code
	e.pressed = pressed
	e.shift_pressed = shift
	Input.parse_input_event(e)


func _tap(code: Key) -> void:
	_key(code, true)
	_key(code, false)


func _pad(button: JoyButton) -> void:
	for p: bool in [true, false]:
		var e := InputEventJoypadButton.new()
		e.button_index = button
		e.pressed = p
		Input.parse_input_event(e)


func _click(at: Vector2, button: MouseButton) -> void:
	var window := get_viewport().get_final_transform() * at
	for p: bool in [true, false]:
		var e := InputEventMouseButton.new()
		e.button_index = button
		e.pressed = p
		e.position = window
		e.global_position = window
		Input.parse_input_event(e)


## One check per step; each waits `_wait` frames for its result.
func _run_checks(hud: Node) -> void:
	if _wait > 0:
		_wait -= 1
		return
	match _step:
		0:
			if Session.pacing().get("paused", false):
				Session.set_paused(false)
			_mark["view"] = int(hud.call(&"view"))
			_tap(KEY_TAB)
			_wait = 10
		1:
			if int(hud.call(&"view")) == int(_mark["view"]):
				_fail("Tab did not switch the view")
			_tap(KEY_TAB)
			_wait = 10
		2:
			if int(hud.call(&"view")) != Bp.VIEW_BRIDGE:
				_fail("Tab did not come back to the bridge view")
			_mark["route"] = (Hud.motion().get("route", PackedVector2Array()) as PackedVector2Array).size()
			var vp := get_viewport().get_visible_rect().size
			_click(Vector2(vp.x * 0.5, vp.y * 0.72), MOUSE_BUTTON_RIGHT)
			_wait = 60
		3:
			var route: PackedVector2Array = Hud.motion().get("route", PackedVector2Array())
			print("u6_capture: route after right-click ", route)
			if route.is_empty():
				_fail("right-clicking the sea did not set a course")
			_tap(KEY_PERIOD)
			_wait = 6
		4:
			_mark["selected"] = Hud.selected_track()
			if Hud.selected_track() == Bp.NONE:
				_fail("the next-track key selected nothing (tracks %d)" % Hud.tracks().size())
			_pad(JOY_BUTTON_A)
			_wait = 60
		5:
			print("u6_capture: designated ", Hud.designated_track(), " selected ", _mark["selected"])
			if int(_mark["selected"]) != Bp.NONE and Hud.designated_track() != int(_mark["selected"]):
				_fail("gamepad A did not designate the selected track")
			_tap(KEY_H)
			_wait = 90
		6:
			print("u6_capture: helm holder after a tap ", Hud.role_holder(Bp.ROLE_HELM, 0))
			if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_PLAYER:
				_fail("a short H press did not latch the helm")
			_tap(KEY_H)
			_wait = 90
		7:
			if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_OFFICER:
				_fail("a second H press did not give the helm back")
			for code: Key in [KEY_LEFT, KEY_UP]:
				_key(code, true)
			_mark["orbit"] = _orbit_yaw(hud)
			_wait = 20
		8:
			for code: Key in [KEY_LEFT, KEY_UP]:
				_key(code, false)
			var focus := get_viewport().gui_get_focus_owner()
			print("u6_capture: yaw ", _mark["orbit"], " -> ", _orbit_yaw(hud), " focus ", focus)
			if focus != null:
				_fail("arrow keys moved focus into %s" % focus.get_path())
			if is_equal_approx(float(_mark["orbit"]), _orbit_yaw(hud)):
				_fail("arrow keys did not orbit the camera")
			if _mode == "skirmish":
				_tap(KEY_P)
			_wait = 20
		9:
			if _mode == "skirmish" and not bool(Session.pacing().get("paused", false)):
				_fail("P did not pause")
			if _mode == "skirmish":
				_tap(KEY_P)
			_key(KEY_W, true)
			_wait = 40
		10:
			print("u6_capture: helm holder with W held ", Hud.role_holder(Bp.ROLE_HELM, 0))
			if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_PLAYER:
				_fail("holding W did not take the helm")
			_key(KEY_W, false)
			_wait = 90
		11:
			# W steps the engine telegraph, which keeps the helm with the player when the key is up;
			# the helm's key hands it back in one press.
			if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_PLAYER:
				_fail("releasing W dropped the telegraph's helm")
			_tap(KEY_H)
			_tap(KEY_L)
			_wait = 90
		12:
			if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_OFFICER:
				_fail("H did not give the helm back")
			_shot_named("hud_checks_log")
			_tap(KEY_L)
			_tap(KEY_TAB)
			_mark["route"] = Hud.motion().get("route", PackedVector2Array())
			_wait = 20
		13:
			if int(hud.call(&"view")) != Bp.VIEW_PLOT:
				_fail("Tab did not open the plot")
			var plot: Control = hud.call(&"plot_view")
			_click(plot.get_global_rect().position + plot.size * Vector2(0.8, 0.3), MOUSE_BUTTON_RIGHT)
			_wait = 60
		14:
			var route: PackedVector2Array = Hud.motion().get("route", PackedVector2Array())
			print("u6_capture: route after a plot right-click ", route)
			if route.is_empty() or route == _mark["route"]:
				_fail("right-clicking the sea on the plot did not set a course")
			var plot: Control = hud.call(&"plot_view")
			_mark["cursor"] = (plot.call(&"plot_cursor") as Dictionary).get("local", Vector2.ZERO)
			var m := InputEventJoypadMotion.new()
			m.axis = JOY_AXIS_RIGHT_X
			m.axis_value = 0.9
			Input.parse_input_event(m)
			_wait = 20
		15:
			var m := InputEventJoypadMotion.new()
			m.axis = JOY_AXIS_RIGHT_X
			m.axis_value = 0.0
			Input.parse_input_event(m)
			var plot: Control = hud.call(&"plot_view")
			var now: Vector2 = (plot.call(&"plot_cursor") as Dictionary).get("local", Vector2.ZERO)
			print("u6_capture: plot cursor ", _mark["cursor"], " -> ", now)
			if now.x <= Vector2(_mark["cursor"]).x + 20.0:
				_fail("the right stick did not move the plot cursor")
			_shot_named("hud_checks_plot")
			_tap(KEY_TAB)
			_key(KEY_TAB, true, true)
			_key(KEY_TAB, false, true)
			_wait = 5
		16:
			var focus := get_viewport().gui_get_focus_owner()
			print("u6_capture: focus after Shift+Tab ", focus)
			if not focus is Tree:
				_fail("Shift+Tab did not move focus to the track table")
			_tap(KEY_RIGHT)
			_wait = 5
		17:
			var focus := get_viewport().gui_get_focus_owner()
			print("u6_capture: focus after Right ", focus.get_path() if focus != null else null)
			if focus == null or not String(focus.get_path()).contains("TrackCard"):
				_fail("Right from the track table did not reach the track card")
			_tap(KEY_RIGHT)
			_wait = 5
		18:
			# Right walks the card's own buttons, then leaves it for the next panel in the chain
			# (weapons, plot tools, alert strip, stations). One Right per pass: input is buffered
			# until the next frame, so focus is read only after a wait.
			var focus := get_viewport().gui_get_focus_owner()
			if focus != null and String(focus.get_path()).contains("TrackCard") and _card_rights < 6:
				_card_rights += 1
				_tap(KEY_RIGHT)
				_wait = 5
				return
			print("u6_capture: focus after leaving the card ", focus.get_path() if focus != null else null)
			if focus == null or String(focus.get_path()).contains("TrackCard") or not String(focus.get_path()).contains("Docks"):
				_fail("Right from the track card did not reach the next panel")
			_tap(KEY_BACKSPACE)
			_wait = 5
		19:
			if get_viewport().gui_get_focus_owner() != null:
				_fail("Backspace did not give focus back to the sortie")
			print("u6_capture: checks done, failures ", _failures)
			_shot_named("hud_checks_end")
			_end()
	_step += 1


func _mouse(at_canvas: Vector2, button: MouseButton, pressed: bool) -> void:
	var window := get_viewport().get_final_transform() * at_canvas
	var e := InputEventMouseButton.new()
	e.button_index = button
	e.pressed = pressed
	e.position = window
	e.global_position = window
	if pressed:
		e.button_mask = MOUSE_BUTTON_MASK_LEFT if button == MOUSE_BUTTON_LEFT else MOUSE_BUTTON_MASK_RIGHT
	Input.parse_input_event(e)


## Real mouse clicks with the button held over several frames, as a hand clicks (scripted mode:
## the orders land in the test feed's command list).
func _run_clicks(hud: Node) -> void:
	if _wait > 0:
		_wait -= 1
		return
	match _step:
		0:
			# Cost of the overlay's per-frame layout with the reference battle on screen.
			var ov := hud.get_node(^"Frame/View/BridgeOverlay") as Control
			var t0 := Time.get_ticks_usec()
			for k in 50:
				ov.call(&"layout_entries")
			print("u6_capture: overlay layout %.3f ms per frame for %d markers" % [float(Time.get_ticks_usec() - t0) / 50000.0, (hud.call(&"world_view").call(&"track_markers") as PackedFloat32Array).size() / 6])
			_feed.call(&"take_commands")
			var w: Node = hud.call(&"world_view")
			var m: PackedFloat32Array = w.call(&"track_markers")
			var own := int(Hud.motion().get("vessel", -1))
			_mark["track"] = -1
			for i in m.size() / 6:
				var t := int(m[i * 6])
				var d: Variant = Hud.track(t)
				var at := Vector2(m[i * 6 + 1], m[i * 6 + 2])
				var overlay := hud.get_node(^"Frame/View/BridgeOverlay") as Control
				if m[i * 6 + 3] > 0.5 and d is Dictionary and int(d["vessel"]) != own and overlay.get_global_rect().grow(-40.0).has_point(at):
					_mark["track"] = t
					_mark["at"] = at
					break
			if int(_mark["track"]) < 0:
				_fail("no on-screen marker to click")
				_step = 3
				return
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, true)
			_wait = 4
		1:
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, false)
			_wait = 10
		2:
			var cmds: PackedStringArray = _feed.call(&"take_commands")
			print("u6_capture: marker click on ", _mark["track"], " sent ", cmds, " selected ", Hud.selected_track())
			if not "\n".join(cmds).contains("Designate"):
				_fail("a left click on a bridge marker did not designate")
			if Hud.selected_track() != int(_mark["track"]):
				_fail("a left click on a bridge marker did not select the track")
		3:
			var b := hud.get_node(^"Frame/Docks/RightDock/Column/Stations/Grid/Damage") as Control
			_mark["at"] = b.get_global_rect().get_center()
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, true)
			_wait = 5
		4:
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, false)
			_wait = 5
		5:
			var station := hud.get_node(^"Frame/Docks/Centre/Margin/Column/Middle/Station") as Control
			print("u6_capture: station after a slow click ", station.visible, " focus ", get_viewport().gui_get_focus_owner())
			if not station.visible:
				_fail("a slow mouse click on a station button did not open it")
			if get_viewport().gui_get_focus_owner() != null:
				_fail("a mouse click left focus in the HUD")
			hud.call(&"close_station")
			if Hud.selected_track() == Bp.NONE:
				Hud.select_step(1)
			_wait = 10
		6:
			var b := hud.get_node(^"Frame/Docks/LeftDock/Column/TrackCard/Scroll/Column/Buttons/Designate") as Button
			_mark["at"] = b.get_global_rect().get_center()
			_mark["label"] = b.text
			_feed.call(&"take_commands")
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, true)
			_wait = 5
		7:
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, false)
			_wait = 10
		8:
			var cmds: PackedStringArray = _feed.call(&"take_commands")
			print("u6_capture: card button (", _mark["label"], ") sent ", cmds)
			if not "\n".join(cmds).contains("Designate"):
				_fail("a slow click on the card's designate button sent nothing")
			hud.call(&"set_view", Bp.VIEW_PLOT)
			_wait = 10
		9:
			var b := hud.get_node(^"Frame/View/PlotTable/Side/Tools/Margin/Column/Layers") as Control
			_mark["at"] = b.get_global_rect().get_center()
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, true)
			_wait = 4
		10:
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, false)
			_wait = 6
		11:
			var grid := hud.get_node(^"Frame/View/PlotTable/Side/Pop/LayersPanel/Margin/Column/Grid")
			var box := grid.get_child(7) as Control
			_mark["at"] = box.get_global_rect().get_center()
			_mark["layers"] = int((hud.call(&"plot_view") as Node).call(&"layers"))
			if not box.is_visible_in_tree():
				_fail("the Layers button did not open the layer panel")
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, true)
			_wait = 4
		12:
			_mouse(_mark["at"], MOUSE_BUTTON_LEFT, false)
			_wait = 6
		13:
			var layers := int((hud.call(&"plot_view") as Node).call(&"layers"))
			print("u6_capture: layers ", _mark["layers"], " -> ", layers)
			if (layers & Bp.PLOT_HISTORY) == (int(_mark["layers"]) & Bp.PLOT_HISTORY):
				_fail("clicking the history checkbox did not toggle the layer")
			_shot_named("hud_clicks_end")
			print("u6_capture: clicks done, failures ", _failures)
			_end()
	_step += 1


func _orbit_yaw(hud: Node) -> float:
	var w: Node = hud.call(&"world_view")
	if w == null:
		return 0.0
	return float((w.call(&"orbit_state") as Dictionary).get("yaw_deg", 0.0))


func _shot_named(name: String) -> void:
	var path := _out.path_join(name + ".png")
	get_viewport().get_texture().get_image().save_png(path)
	print("u6_capture: saved ", path)


func _fail(text: String) -> void:
	push_error("u6_capture: " + text)
	_failures += 1


func _end() -> void:
	_active = false
	if _feed != null:
		_feed.call(&"stop")
	else:
		Session.leave(false)
	get_tree().quit(1 if _failures > 0 else 0)
