extends Control
## Replay playback controls and camera choice; also plays kill-cam clips.
## DESIGN.md L1043-1050. Unit: ui-replays.
## Reads and calls: Session.replay_info, Session.replay_seek, Session.replay_set_rate, Session.replay_set_viewer, BpWorldView.follow, BpWorldView.set_free_camera.
##
## `Screens` builds it into the replay root (world, these controls, the HUD) and never calls `setup`,
## so the controls start themselves: they listen to the session and stay hidden while no replay is
## open (`Session.replay_info()` empty). The root passes the mouse through; only the bar at the
## bottom takes clicks, so the world and HUD keep theirs.
##
## The bar, most used first: Pause or Play, the time against the replay's length, the seek bar
## (keyframes every 60 s make seeking quick, L1045), the playback rate (0.25x to 8x), then who the
## replay is watched as (ground truth or one side's picture, where the replay allows it; a PvP
## replay gives only each side's own view, L1049), the camera (the default orbit, follow any drawn
## vessel, or a free camera the helm controls move and the look controls turn and lift), Pin (kept
## past the 20 most recent, L1047) and Leave replay. The pause action pauses and plays, and the
## time-rate actions step the rate; the seek bar takes keyboard and gamepad steps when focused.
##
## Kill cam: `play_kill_cam(clip)` takes the Dictionary of `Session.kill_cam_ready` (`victim`,
## `killer`, `from_s`, `to_s`) and shows what happened (L1049): in a replay it plays that window
## and pauses at its end; over a live sortie the session plays the clip from its recording. A frame
## names it Kill cam with the window's clock, follows the killer, and Back returns to where the
## player was. When this scene is instanced over a live sortie it plays clips as they arrive.
## Seek-bar and clock text redraw on `Session.replay_position_changed`, not every frame.

const Kit := preload("res://ui/screens/menu_kit.gd")
const ReplayBrowser := preload("res://ui/screens/replay_browser.gd")

const RATES: Array[float] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0]
## Free camera: metres per second at 1x, and its height limits.
const FREE_SPEED_MPS := 60.0
const FREE_TURN_DEG_PER_S := 60.0
const FREE_MIN_HEIGHT_M := 4.0
const FREE_MAX_HEIGHT_M := 3000.0
const FREE_START_HEIGHT_M := 120.0
const FREE_START_BACK_M := 250.0
## `Bp.SESSION_*`.
const SESSION_REPLAY := 5
const SESSION_IN_SORTIE := 3
const SESSION_ENDED := 4
const CAMERA_DEFAULT := 0
const CAMERA_FREE := 1
const CAMERA_FOLLOW := 2

var _bar: PanelContainer
var _play: Button
var _clock: Label
var _seek: HSlider
var _rate: OptionButton
var _viewer: OptionButton
var _camera: OptionButton
var _pin: Button
var _leave: Button
var _free_hint: Label
var _kill_frame: PanelContainer
var _kill_title: Label
var _kill_text: Label
var _kill_back: Button

var _info: Dictionary = {}
var _seeking := false
var _setting := false
var _free := false
var _free_pos := Vector2.ZERO
var _free_height := FREE_START_HEIGHT_M
var _free_yaw_deg := 0.0
## The vessel the follow camera holds (`Bp.NONE` = none); the camera list keeps it selected.
var _followed := -1
var _focused_once := false
## Camera menu rows: `{ "kind": int, "vessel": int }`.
var _camera_rows: Array[Dictionary] = []
var _clip: Dictionary = {}
var _clip_live := false
var _bound := false
var _path := ""


func _ready() -> void:
	set_anchors_preset(Control.PRESET_FULL_RECT)
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	accessibility_name = tr(&"replay.controls")
	# A full-rect column that passes the mouse through, with the bar at its foot.
	var frame := VBoxContainer.new()
	frame.set_anchors_preset(Control.PRESET_FULL_RECT)
	frame.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(frame)
	var spacer_top := Control.new()
	spacer_top.size_flags_vertical = Control.SIZE_EXPAND_FILL
	spacer_top.mouse_filter = Control.MOUSE_FILTER_IGNORE
	frame.add_child(spacer_top)
	_bar = PanelContainer.new()
	_bar.mouse_filter = Control.MOUSE_FILTER_STOP
	frame.add_child(_bar)
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 3)
	_bar.add_child(margin)
	var column := Kit.vbox(2)
	margin.add_child(column)

	var top := Kit.hbox(3)
	column.add_child(top)
	_play = _button("replay.pause", 140.0)
	_play.pressed.connect(_toggle_pause)
	top.add_child(_play)
	_clock = Kit.label("", &"Body", true)
	_clock.custom_minimum_size = Vector2(220.0, 0.0)
	_clock.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_clock.autowrap_mode = TextServer.AUTOWRAP_OFF
	top.add_child(_clock)
	_seek = preload("res://ui/widgets/focus_slider.gd").new()
	_seek.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_seek.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_seek.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	_seek.step = 1.0
	_seek.focus_mode = Control.FOCUS_ALL
	_seek.accessibility_name = tr(&"replay.seek")
	_seek.drag_started.connect(func() -> void: _seeking = true)
	_seek.drag_ended.connect(_on_seek_drag_ended)
	_seek.value_changed.connect(_on_seek_value)
	top.add_child(_seek)
	_rate = _option(150.0, &"replay.rate")
	for i in RATES.size():
		_rate.add_item(rate_text(RATES[i]), i)
	_rate.item_selected.connect(_on_rate)
	top.add_child(_rate)

	var bottom := Kit.hbox(3)
	column.add_child(bottom)
	bottom.add_child(_caption("replay.viewer"))
	_viewer = _option(240.0, &"replay.viewer")
	_viewer.item_selected.connect(_on_viewer)
	bottom.add_child(_viewer)
	bottom.add_child(_caption("replay.camera"))
	_camera = _option(260.0, &"replay.camera")
	_camera.item_selected.connect(_on_camera)
	# The vessels drawn change as the replay plays: the list is read when it opens, not per frame.
	_camera.get_popup().about_to_popup.connect(_fill_cameras)
	bottom.add_child(_camera)
	_free_hint = Kit.label("replay.free_hint", &"Dense")
	_free_hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_free_hint.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_free_hint.visible = false
	bottom.add_child(_free_hint)
	var spacer := Control.new()
	spacer.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	spacer.mouse_filter = Control.MOUSE_FILTER_IGNORE
	bottom.add_child(spacer)
	_pin = _button("replay.pin", 160.0)
	_pin.icon = Ui.icon(&"pin")
	_pin.pressed.connect(_toggle_pin)
	bottom.add_child(_pin)
	_leave = _button("replay.leave", 180.0)
	_leave.pressed.connect(_on_leave)
	bottom.add_child(_leave)

	_kill_frame = PanelContainer.new()
	_kill_frame.set_anchors_preset(Control.PRESET_CENTER_TOP)
	_kill_frame.grow_horizontal = Control.GROW_DIRECTION_BOTH
	_kill_frame.offset_top = Ui.spacing(5)
	_kill_frame.mouse_filter = Control.MOUSE_FILTER_STOP
	_kill_frame.visible = false
	add_child(_kill_frame)
	var km := MarginContainer.new()
	Ui.set_margins(km, 3)
	_kill_frame.add_child(km)
	var kc := Kit.hbox(4)
	km.add_child(kc)
	var ktext := Kit.vbox(0)
	_kill_title = Kit.label("replay.kill_cam", &"PanelTitle")
	ktext.add_child(_kill_title)
	_kill_text = Kit.label("", &"Body", true)
	_kill_text.autowrap_mode = TextServer.AUTOWRAP_OFF
	ktext.add_child(_kill_text)
	kc.add_child(ktext)
	_kill_back = _button("replay.kill_back", 220.0)
	_kill_back.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_kill_back.pressed.connect(stop_kill_cam)
	kc.add_child(_kill_back)

	_link_focus()
	visible = false
	set_process(false)
	_bind.call_deferred()


func _button(key: String, width: float) -> Button:
	var b := Kit.inline_button(key)
	b.custom_minimum_size = Vector2(width, Kit.BUTTON_H)
	b.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	return b


func _option(width: float, label_key: StringName) -> OptionButton:
	var o := OptionButton.new()
	o.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	o.custom_minimum_size = Vector2(width, Kit.BUTTON_H)
	o.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	o.focus_mode = Control.FOCUS_ALL
	o.accessibility_name = tr(label_key)
	return o


func _caption(key: String) -> Label:
	var l := Kit.label(key, &"Body")
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	return l


## Start listening to the session. Runs once, a frame after entering the tree.
func _bind() -> void:
	if _bound or not is_inside_tree():
		return
	var session := Kit.autoload(&"Session")
	if session == null:
		return
	_bound = true
	for pair: Array in [[&"replay_position_changed", _on_position], [&"state_changed", _on_state], [&"kill_cam_ready", _on_kill_cam_ready]]:
		if session.has_signal(pair[0]) and not session.is_connected(pair[0], pair[1]):
			session.connect(pair[0], pair[1])
	_path = _replay_path()
	refresh()


func _replay_path() -> String:
	if not ReplayBrowser.opened_path.is_empty():
		return ReplayBrowser.opened_path
	var cmd := Kit.as_dict(Kit.call_on(&"App", &"command_line"))
	return Kit.get_str(cmd, "replay")


func _on_state(_state: int) -> void:
	refresh()


func _on_position(_tick: int) -> void:
	refresh()


## Read `Session.replay_info` and redraw.
func refresh() -> void:
	if not is_inside_tree():
		return
	show_info(Kit.as_dict(Kit.call_on(&"Session", &"replay_info")))


## Render a `replay_info` Dictionary (empty = no replay open: the bar hides).
func show_info(info: Dictionary) -> void:
	_info = info
	var open := not info.is_empty()
	_bar.visible = open
	visible = open or _kill_frame.visible
	set_process(open or not _clip.is_empty())
	if not open:
		_set_free(false)
		return
	_setting = true
	var duration := maxf(Kit.get_float(info, "duration_s"), 0.0)
	var t := clampf(Kit.get_float(info, "time_s"), 0.0, duration)
	_seek.max_value = maxf(duration, 1.0)
	if not _seeking:
		_seek.value = t
	_clock.text = clock_line(t, duration)
	var paused := Kit.get_bool(info, "paused")
	_play.text = "replay.play" if paused else "replay.pause"
	var rate := Kit.get_float(info, "rate", 1.0)
	_rate.select(closest_rate(rate))
	_fill_viewers(info)
	var pinned := Kit.get_bool(info, "pinned")
	_pin.text = "replay.unpin" if pinned else "replay.pin"
	_pin.visible = not _path.is_empty()
	_setting = false
	if _camera.item_count == 0:
		_fill_cameras()
	# Keyboard and gamepad players start on the bar; the mouse does not need it.
	if not _focused_once and get_viewport().gui_get_focus_owner() == null:
		_focused_once = true
		Ui.focus(_play)


static func rate_text(rate: float) -> String:
	var decimals := 2 if rate < 0.5 else (1 if rate < 1.0 else 0)
	return Kit.tr_key(&"replay.rate_row").format([Kit.number(rate, &"none", decimals)])


static func closest_rate(rate: float) -> int:
	var best := 2
	for i in RATES.size():
		if absf(RATES[i] - rate) < absf(RATES[best] - rate):
			best = i
	return best


static func clock_line(t: float, duration: float) -> String:
	return Kit.tr_key(&"replay.clock").format([ReplayBrowser.clock_text(t), ReplayBrowser.clock_text(duration)])


func _fill_viewers(info: Dictionary) -> void:
	var sides: Array[int] = []
	var raw: Variant = info.get("sides", PackedInt32Array())
	for s: Variant in (Array(raw) if raw is PackedInt32Array else Kit.as_array(raw)):
		sides.append(int(s))
	var current := Kit.get_int(info, "viewer_side", -1)
	var ids: Array[int] = []
	for i in _viewer.item_count:
		ids.append(_viewer.get_item_id(i))
	var want: Array[int] = []
	# Ground truth is offered unless the replay only allows side views (PvP, L1049); the session
	# refuses a viewer the replay does not hold, so the list mirrors what it accepts.
	want.append(-1)
	for s: int in sides:
		want.append(s)
	if ids != want:
		_viewer.clear()
		for v: int in want:
			_viewer.add_item(Kit.tr_key(&"replay.viewer.truth") if v < 0 else Kit.tr_key(&"replay.viewer.side").format([v + 1]), v)
	var idx := _viewer.get_item_index(current)
	if idx >= 0:
		_viewer.select(idx)


func _fill_cameras() -> void:
	_camera_rows = [{"kind": CAMERA_DEFAULT, "vessel": -1}, {"kind": CAMERA_FREE, "vessel": -1}]
	var labels: Array[String] = [Kit.tr_key(&"replay.camera.default"), Kit.tr_key(&"replay.camera.free")]
	var seen: Dictionary[int, bool] = {}
	for a: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"allies")):
		var v := Kit.get_int(a, "vessel", -1)
		if v >= 0 and not seen.has(v):
			seen[v] = true
			_camera_rows.append({"kind": CAMERA_FOLLOW, "vessel": v})
			var n := Kit.get_str(a, "name")
			labels.append(Kit.tr_key(&"replay.camera.follow").format([n if not n.is_empty() else Kit.tr_key(&"replay.camera.ally")]))
	for tr_d: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"tracks")):
		var v := Kit.get_int(tr_d, "vessel", -1)
		if v >= 0 and not seen.has(v):
			seen[v] = true
			_camera_rows.append({"kind": CAMERA_FOLLOW, "vessel": v})
			labels.append(Kit.tr_key(&"replay.camera.follow_track").format([Kit.get_str(tr_d, "label")]))
	_camera.clear()
	var pick := CAMERA_FREE if _free else CAMERA_DEFAULT
	for i in labels.size():
		_camera.add_item(labels[i], i)
		if not _free and _followed >= 0 and Kit.get_int(_camera_rows[i], "vessel", -1) == _followed:
			pick = i
	_camera.select(pick)


# --- controls -----------------------------------------------------------------------------------

func _toggle_pause() -> void:
	Kit.call_on(&"Session", &"replay_set_paused", [not Kit.get_bool(_info, "paused")])
	refresh()


func _on_seek_drag_ended(changed: bool) -> void:
	_seeking = false
	if changed:
		Kit.call_on(&"Session", &"replay_seek", [_seek.value])
	refresh()


func _on_seek_value(v: float) -> void:
	# Keyboard and pad steps seek at once; a mouse drag seeks when it ends.
	if _setting or _seeking:
		_clock.text = clock_line(v, Kit.get_float(_info, "duration_s"))
		return
	Kit.call_on(&"Session", &"replay_seek", [v])


func _on_rate(index: int) -> void:
	if _setting:
		return
	Kit.call_on(&"Session", &"replay_set_rate", [RATES[clampi(index, 0, RATES.size() - 1)]])
	refresh()


func _on_viewer(index: int) -> void:
	if _setting:
		return
	var side := _viewer.get_item_id(index)
	if Kit.call_on(&"Session", &"replay_set_viewer", [side]) != true:
		refresh()
	else:
		# What is drawn changes with the viewer; the follow list follows it.
		_fill_cameras.call_deferred()


func _on_camera(index: int) -> void:
	if index < 0 or index >= _camera_rows.size():
		return
	var row := _camera_rows[index]
	var view := _world_view()
	match Kit.get_int(row, "kind"):
		CAMERA_DEFAULT:
			_set_free(false)
			_followed = -1
			if view != null:
				view.call(&"follow", -1)
		CAMERA_FREE:
			_followed = -1
			_set_free(true)
		CAMERA_FOLLOW:
			_set_free(false)
			_followed = Kit.get_int(row, "vessel")
			if view != null and view.call(&"follow", _followed) != true:
				# No longer drawn: back to the default camera, and the list is read again.
				_followed = -1
				view.call(&"follow", -1)
				_fill_cameras()


func _world_view() -> Node:
	var screens := Kit.autoload(&"Screens")
	return screens.call(&"world_view") if screens != null else null


func _set_free(on: bool) -> void:
	if on == _free:
		return
	_free = on
	_free_hint.visible = on
	var view := _world_view()
	if on and view != null:
		var orbit := Kit.as_dict(view.call(&"orbit_state"))
		_free_yaw_deg = Kit.get_float(orbit, "yaw_deg")
		var origin: Variant = view.call(&"render_origin")
		var centre: Vector2 = origin if origin is Vector2 else Vector2.ZERO
		_free_pos = centre - _forward() * FREE_START_BACK_M
		_free_height = FREE_START_HEIGHT_M
		_apply_free()
	elif view != null:
		view.call(&"set_free_camera", false, Vector2.ZERO, 0.0, Vector2.ZERO, 0.0)
	set_process(not _info.is_empty() or not _clip.is_empty())


## Map direction of the free camera's heading: bearing 0 is north (-z), clockwise.
func _forward() -> Vector2:
	var r := deg_to_rad(_free_yaw_deg)
	return Vector2(sin(r), -cos(r))


func _apply_free() -> void:
	var view := _world_view()
	if view == null:
		return
	var look := _free_pos + _forward() * FREE_START_BACK_M
	view.call(&"set_free_camera", true, _free_pos, _free_height, look, 0.0)


func _process(delta: float) -> void:
	if _free and not _covered():
		_move_free(delta)
	if not _clip.is_empty():
		_show_clip_clock()


## A screen or dialog over the replay takes the keys; the free camera holds still under it.
func _covered() -> bool:
	var screens := Kit.autoload(&"Screens")
	if screens == null:
		return false
	return int(screens.call(&"depth")) > 1 or int(screens.call(&"modal_count")) > 0


func _move_free(delta: float) -> void:
	if get_viewport().gui_get_focus_owner() is LineEdit:
		return
	# The sortie's own actions, so the camera follows the player's bindings and the gamepad sticks:
	# the helm moves it, look left and right turn it, look up and down climb and descend.
	var fwd := _forward()
	var right := Vector2(-fwd.y, fwd.x)
	var move := fwd * (Input.get_action_strength(&"helm_ahead") - Input.get_action_strength(&"helm_astern")) \
		+ right * (Input.get_action_strength(&"helm_starboard") - Input.get_action_strength(&"helm_port"))
	var turn := Input.get_action_strength(&"look_right") - Input.get_action_strength(&"look_left")
	var climb := Input.get_action_strength(&"look_up") - Input.get_action_strength(&"look_down")
	if move.length() < 0.05 and absf(turn) < 0.05 and absf(climb) < 0.05:
		return
	# Faster when higher, so the sea crosses the screen at a similar pace at any height.
	var speed := FREE_SPEED_MPS * maxf(1.0, _free_height / FREE_START_HEIGHT_M)
	_free_pos += move.limit_length(1.0) * speed * delta
	_free_yaw_deg = fposmod(_free_yaw_deg + turn * FREE_TURN_DEG_PER_S * delta, 360.0)
	_free_height = clampf(_free_height + climb * speed * delta, FREE_MIN_HEIGHT_M, FREE_MAX_HEIGHT_M)
	_apply_free()


func _unhandled_input(event: InputEvent) -> void:
	if not _bar.visible or not is_visible_in_tree() or _covered():
		return
	# The sortie's pause and time-rate actions drive playback (rebindable, gamepad included).
	if event.is_action_pressed(&"pause", false, true):
		_toggle_pause()
		get_viewport().set_input_as_handled()
	elif event.is_action_pressed(&"time_faster", false, true) or event.is_action_pressed(&"time_slower", false, true):
		var step := 1 if event.is_action_pressed(&"time_faster", false, true) else -1
		var i := clampi(closest_rate(Kit.get_float(_info, "rate", 1.0)) + step, 0, RATES.size() - 1)
		Kit.call_on(&"Session", &"replay_set_rate", [RATES[i]])
		refresh()
		get_viewport().set_input_as_handled()


func _toggle_pin() -> void:
	if _path.is_empty():
		return
	var ok: Variant = Kit.call_on(&"Session", &"replay_pin", [_path, not Kit.get_bool(_info, "pinned")])
	if ok != true:
		Screens.open_message(&"replays.pin_failed_title", &"replays.pin_failed_body")
	refresh()


func _on_leave() -> void:
	_set_free(false)
	ReplayBrowser.opened_path = ""
	# The session going idle returns the screens to the main menu.
	Kit.call_on(&"Session", &"leave", [false])


# --- kill cam -----------------------------------------------------------------------------------

func _on_kill_cam_ready(clip: Dictionary) -> void:
	# Only over a live sortie: in a replay the player watches the whole replay instead.
	var state: Variant = Kit.call_on(&"Session", &"state")
	if state is int and (int(state) == SESSION_IN_SORTIE or int(state) == SESSION_ENDED):
		play_kill_cam(clip)


## Play a kill-cam clip (`victim`, `killer`, `from_s`, `to_s`) and show its frame.
func play_kill_cam(clip: Dictionary) -> bool:
	var from_s := Kit.get_float(clip, "from_s", -1.0)
	var to_s := Kit.get_float(clip, "to_s", -1.0)
	if from_s < 0.0 or to_s < from_s:
		return false
	if Kit.call_on(&"Session", &"kill_cam_play", [clip]) != true:
		return false
	_clip = clip
	_clip_live = _info.is_empty()
	var view := _world_view()
	var killer := Kit.get_int(clip, "killer", -1)
	if view != null and killer >= 0:
		view.call(&"follow", killer)
	show_kill_frame(clip, _clip_live)
	set_process(true)
	return true


## Show the kill-cam frame for a clip (also the capture entry point).
func show_kill_frame(clip: Dictionary, live: bool) -> void:
	_clip = clip
	_clip_live = live
	_kill_back.text = "replay.kill_back_live" if live else "replay.kill_back"
	_kill_frame.visible = true
	visible = true
	_show_clip_clock()
	Ui.focus(_kill_back)


func _show_clip_clock() -> void:
	var from_s := Kit.get_float(_clip, "from_s")
	var to_s := Kit.get_float(_clip, "to_s")
	var now: Variant = Kit.call_on(&"Session", &"time_s")
	var t := clampf(float(now) if (now is float or now is int) else from_s, from_s, to_s)
	_kill_text.text = Kit.tr_key(&"replay.kill_window").format([ReplayBrowser.clock_text(t - from_s), ReplayBrowser.clock_text(to_s - from_s)])


func stop_kill_cam() -> void:
	Kit.call_on(&"Session", &"kill_cam_stop")
	var view := _world_view()
	if view != null:
		view.call(&"follow", -1)
	_clip = {}
	_kill_frame.visible = false
	visible = _bar.visible
	set_process(not _info.is_empty())
	refresh()


func _link_focus() -> void:
	Kit.chain_horizontal([_play, _seek, _rate])
	Kit.chain_horizontal([_viewer, _camera, _pin, _leave])
	for c: Control in [_play, _seek, _rate]:
		c.focus_neighbor_bottom = c.get_path_to(_viewer)
		c.focus_neighbor_top = c.get_path_to(c)
	for c: Control in [_viewer, _camera, _pin, _leave]:
		c.focus_neighbor_top = c.get_path_to(_play)
		c.focus_neighbor_bottom = c.get_path_to(c)
