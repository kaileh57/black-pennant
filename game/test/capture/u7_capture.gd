extends Node
## Windowed captures and checks of the Bridge Board, officer lines, Why?, the control station, the
## alert strip and the log (units ui-hud-board, ui-hud-control, ui-hud-alerts). Runs the real
## Screens routing for a live skirmish or a scripted BpTestFeed picture, then works through shots:
##   godot --path game --resolution 1600x900 res://test/capture/u7_capture.tscn -- \
##     --mode=skirmish|scripted --script=battle --shots=board,helm,diff,why,control_doctrine \
##     --palette=dusk --scale=1 --frames=240 --out=/abs/dir --prefix=u7
## Each shot sets up a state through the same calls a player's input makes, waits for the round
## trip, saves <prefix>_<shot>.png and checks what it can; it exits 1 when a check failed.
## Headless, or when instanced by check_project, it does nothing.

const STARTER := "starters/fac22.bp.ron"
const LIMIT := 9000
const RULES_PATH := "user://blueprints/u7_rules_capture.bp.ron"

var _mode := "skirmish"
var _script := "battle"
var _shots: PackedStringArray = ["board"]
var _palette := "dusk"
var _scale := 1.0
var _frames := 240
var _out := "/tmp"
var _prefix := "u7"
var _skip := 0
var _rules := false
var _lines := ""
var _skipped := false
var _feed: Node = null
var _frame := 0
var _sortie_frame := -1
var _active := false
var _failures := 0
var _shot := 0
var _phase := 0
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
			"shots": _shots = kv[1].split(",", false)
			"palette": _palette = kv[1]
			"scale": _scale = float(kv[1])
			"frames": _frames = int(kv[1])
			"out": _out = kv[1]
			"prefix": _prefix = kv[1]
			"skip": _skip = int(kv[1])
			"rules": _rules = kv[1] == "1"
			"lines": _lines = kv[1]
	DirAccess.make_dir_recursive_absolute(_out)
	Settings.reload()
	Ui.refresh()
	Ui.call(&"_apply_palette", StringName(_palette))
	Ui.call(&"_apply_scale", _scale)
	if not _lines.is_empty():
		var parts := _lines.split(":")
		Ui.call(&"_apply_officer_lines", float(parts[0]), float(parts[1]) if parts.size() > 1 else 1.0)
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
		var blueprint := STARTER
		if _rules:
			blueprint = _with_rules()
		var problems := Session.start_skirmish({
			"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_1, "enemy_per_player": 3, "difficulty": {"preset": "normal"},
			"weather": Bp.WEATHER_CLEAR, "sea_state": 3, "time_of_day": Bp.TIME_DUSK, "seed": 1234, "blueprint": blueprint,
		})
		if not problems.is_empty():
			_fail("start_skirmish refused: " + str(problems))
			get_tree().quit(1)
			return
	_active = true


## The starter with an L2 rules table, written to user:// for this run.
func _with_rules() -> String:
	var text := FileAccess.get_file_as_string(ProjectSettings.globalize_path("res://").path_join("../blueprints").path_join(STARTER))
	var rules := "rules: [\n" \
		+ "        (when: Compare(lhs: OwnSpeedKn, cmp: Gt, rhs: Const(5.0)), sink: ShipEmcon, value: Set(Emcon(Silent)), enabled: true),\n" \
		+ "        (when: Compare(lhs: NearestContactRangeM((min_stance: Suspect, domain: None, min_grade: Bearing)), cmp: Lt, rhs: Const(30000.0)), sink: ShipPreset, value: Set(Preset(Aggressive)), enabled: true),\n" \
		+ "        (when: Compare(lhs: HullHealth, cmp: Lt, rhs: Const(0.5)), sink: HelmSpeed, value: Set(Scalar(30.0)), enabled: false),\n" \
		+ "    ],"
	text = text.replace("rules: [],", rules)
	DirAccess.make_dir_recursive_absolute(ProjectSettings.globalize_path("user://blueprints"))
	var path := RULES_PATH
	var f := FileAccess.open(path, FileAccess.WRITE)
	f.store_string(text)
	f.close()
	return path


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _feed != null:
		_feed.call(&"advance", 1)
	if _frame > LIMIT:
		_fail("ran out of frames at shot %d" % _shot)
		_end()
		return
	if _sortie_frame < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE and not Screens.is_loading():
			_sortie_frame = _frame
		elif Session.state() == Bp.SESSION_FAILED:
			_fail("the session failed")
			_end()
		return
	if not _skipped and _feed != null and _skip > 0 and _frame - _sortie_frame > 30:
		# Jump the scripted sortie forward (one advance holds at most an hour of ticks).
		_skipped = true
		_feed.call(&"advance", _skip)
	if _frame - _sortie_frame < _frames:
		return
	var hud := _hud()
	if hud == null:
		_fail("no HUD on the stack")
		_end()
		return
	if _wait > 0:
		_wait -= 1
		return
	if _shot >= _shots.size():
		print("u7_capture: done, failures ", _failures)
		_end()
		return
	_step(hud, _shots[_shot])


func _hud() -> Node:
	var top: Node = Screens.top()
	if top != null and top.has_method(&"toggle_view"):
		return top
	return null


func _board(hud: Node) -> Control:
	return hud.get_node(^"Frame/Docks/RightDock/Column/BridgeBoard")


func _control(hud: Node) -> Control:
	return hud.get_node(^"Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Control")


func _log(hud: Node) -> Control:
	return hud.get_node(^"Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Log")


func _strip(hud: Node) -> Control:
	return hud.get_node(^"Frame/Docks/Centre/Margin/Column/Top/Middle/AlertStrip")


func _why(hud: Node) -> Control:
	return hud.get_node(^"Frame/Overlays/WhyPopup")


func _next_shot(wait: int) -> void:
	_shot += 1
	_phase = 0
	_wait = wait


## Phase 0 sets the state up, phase 1 checks and shoots, phase 2 (when needed) tears it down.
func _step(hud: Node, shot: String) -> void:
	match shot:
		"board", "strip":
			if shot == "strip":
				var st := Hud.alert_strip()
				print("u7_capture: strip more=", st["more"], " flood=", st["flood"], " next_ack=", st["next_ack"], " shown=", (st["shown"] as Array).map(func(a: Dictionary) -> String: return "%d/%d/%d" % [a["id"], a["priority"], a["state"]]))
				print("u7_capture: all alerts ", Hud.alerts(false).map(func(a: Dictionary) -> String: return "%d/%d/%d" % [a["id"], a["priority"], a["state"]]))
			_save(shot)
			_next_shot(0)
		"helm":
			if _phase == 0:
				Orders.take_control(Bp.ROLE_HELM, 0, true)
				_phase = 1
				_wait = 45
			elif _phase == 1:
				if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_PLAYER:
					_fail("the helm was not taken")
				_save(shot)
				Orders.take_control(Bp.ROLE_HELM, 0, false)
				_next_shot(30)
		"click_take":
			# Press the board's Take helm button with the mouse, as a player does.
			var board := _board(hud)
			if _phase == 0:
				var button: Button = null
				for b: Node in board.find_children("*", "Button", true, false):
					if (b as Button).toggle_mode and String(b.get_parent().name).begins_with("Role_0"):
						button = b
				if button == null:
					_fail("no helm takeover button on the board")
					_next_shot(0)
					return
				var at := button.get_global_rect().get_center()
				_mouse(at, true)
				_mark["at"] = at
				_phase = 1
				_wait = 3
			elif _phase == 1:
				_mouse(_mark["at"], false)
				_phase = 2
				_wait = 45
			else:
				if Hud.role_holder(Bp.ROLE_HELM, 0) != Bp.HOLDER_PLAYER:
					_fail("clicking Take helm did not take the helm")
				_save(shot)
				Orders.take_control(Bp.ROLE_HELM, 0, false)
				_next_shot(30)
		"diff":
			if _phase == 0:
				Orders.set_slider("gunnery.aggression", 0.9)
				Orders.set_emcon(Bp.EMCON_ACTIVE)
				_phase = 1
				_wait = 30
			elif _phase == 1:
				var g: Button = _board(hud).get_node(^"Scroll/Margin/Groups/Officer3/Overrides")
				if not g.visible:
					_fail("the gunnery overrides button did not appear after a slider change")
				g.emit_signal(&"pressed")
				_phase = 2
				_wait = 20
			else:
				var diff: Control = _board(hud).get_node(^"Scroll/Margin/Groups/Officer3/Diff")
				if diff.get_child_count() < 2:
					_fail("the diff did not fill (children %d)" % diff.get_child_count())
				_save(shot)
				_next_shot(0)
		"revert":
			if _phase == 0:
				var diff: Control = _board(hud).get_node(^"Scroll/Margin/Groups/Officer3/Diff")
				var all := diff.get_node_or_null(^"RevertAll") as Button
				if all == null:
					_fail("no Revert all in the diff")
					_next_shot(0)
					return
				all.emit_signal(&"pressed")
				Orders.revert("emcon")
				_phase = 1
				_wait = 40
			else:
				var s := Kit_dicts(Hud.sliders(Bp.OFFICER_GUNNERY))
				for d: Dictionary in s:
					if d["key"] == "gunnery.aggression" and bool(d["overridden"]):
						_fail("Revert all left gunnery.aggression overridden")
				if bool(Hud.emcon().get("overridden", false)):
					_fail("reverting emcon by its id did not revert it")
				_save(shot)
				_next_shot(0)
		"why":
			if _phase == 0:
				var slot := -1
				for m: Dictionary in Hud.mounts():
					slot = int(m["slot"])
					break
				if slot < 0:
					_fail("no mount to ask Why? of")
					_next_shot(0)
					return
				_why(hud).call(&"ask", slot, get_viewport().get_visible_rect().size * Vector2(0.45, 0.4))
				_phase = 1
				_wait = 20
			else:
				if not bool(_why(hud).call(&"is_open")):
					_fail("Why? did not open")
				_save(shot)
				_why(hud).call(&"close")
				_next_shot(0)
		"log":
			if _phase == 0:
				if StringName(hud.call(&"station")) != &"log":
					hud.call(&"open_station", &"log")
				_phase = 1
				_wait = 20
			else:
				_save(shot)
				_phase = 0
				_next_shot(0)
		"log_read":
			if _phase == 0:
				if StringName(hud.call(&"station")) != &"log":
					hud.call(&"open_station", &"log")
				_mark["unread"] = Hud.log_unread()
				_phase = 1
				_wait = 150
			else:
				print("u7_capture: unread before ", _mark["unread"], " after ", Hud.log_unread())
				if int(_mark["unread"]) > 0 and Hud.log_unread() >= int(_mark["unread"]):
					_fail("an open log did not mark its entries read")
				_save(shot)
				_next_shot(0)
		"ack":
			if _phase == 0:
				_mark["next"] = int(Hud.alert_strip().get("next_ack", -1))
				var b: Button = _strip(hud).get_node(^"Margin/Column/Actions/Acknowledge")
				b.emit_signal(&"pressed")
				_phase = 1
				_wait = 30
			else:
				var a: Variant = Hud.alert(int(_mark["next"]))
				print("u7_capture: acknowledged ", _mark["next"], " -> ", a)
				if int(_mark["next"]) >= 0 and a is Dictionary and int(a["state"]) == Bp.ALERT_UNACKNOWLEDGED:
					_fail("Acknowledge left its alert unacknowledged")
				_save(shot)
				_next_shot(0)
		"silence":
			if _phase == 0:
				var b: Button = _strip(hud).get_node(^"Margin/Column/Actions/Silence")
				b.emit_signal(&"pressed")
				_phase = 1
				_wait = 60
			else:
				if float(Hud.alert_strip().get("silenced_s", 0.0)) <= 0.0:
					_fail("Silence did not silence")
				_save(shot)
				_next_shot(0)
		"focus":
			# Keyboard path: Shift+Tab into the docks, Right to the station buttons, Up into the
			# Bridge Board, Up along it, Left back to the track table.
			var f := get_viewport().gui_get_focus_owner()
			var path := String(f.get_path()) if f != null else ""
			match _phase:
				0:
					_key(KEY_TAB, true, true)
					_key(KEY_TAB, false, true)
				1:
					if not f is Tree:
						_fail("Shift+Tab did not reach the track table")
					_key(KEY_RIGHT, true)
					_key(KEY_RIGHT, false)
				2:
					if not path.contains("Stations"):
						_fail("Right from the track table did not reach the station buttons: " + path)
					_key(KEY_UP, true)
					_key(KEY_UP, false)
				3:
					if not path.contains("BridgeBoard"):
						_fail("Up from the station buttons did not reach the Bridge Board: " + path)
					_mark["first"] = f
					_key(KEY_UP, true)
					_key(KEY_UP, false)
				4:
					if f == _mark["first"] or not path.contains("BridgeBoard"):
						_fail("Up did not move along the Bridge Board: " + path)
					_save(shot)
					_key(KEY_LEFT, true)
					_key(KEY_LEFT, false)
				_:
					if not f is Tree:
						_fail("Left from the Bridge Board did not return to the track table: " + path)
					_key(KEY_BACKSPACE, true)
					_key(KEY_BACKSPACE, false)
					_next_shot(5)
					return
			print("u7_capture: focus step ", _phase, " ", path)
			_phase += 1
			_wait = 5
		"ids":
			_ids(hud)
		"perf":
			# Main-thread cost of a full Bridge Board and alert strip redraw (2 ms adapter budget).
			var board := _board(hud)
			var t0 := Time.get_ticks_usec()
			for k in 50:
				board.call(&"_render")
			var board_ms := float(Time.get_ticks_usec() - t0) / 50000.0
			t0 = Time.get_ticks_usec()
			for k in 50:
				_strip(hud).call(&"refresh")
			var strip_ms := float(Time.get_ticks_usec() - t0) / 50000.0
			print("u7_capture: board redraw %.3f ms, strip redraw %.3f ms" % [board_ms, strip_ms])
			_next_shot(0)
		"rightclick":
			# Right-click a part of the own hull in the bridge view: Why? opens for it.
			if _phase == 0:
				var w: Node = hud.call(&"world_view")
				var view: Control = hud.get_node(^"Frame/View")
				var r := view.get_global_rect()
				_mark["at"] = Vector2(-1, -1)
				for yy in range(0, 21):
					for xx in range(0, 21):
						var p := r.position + r.size * Vector2(0.25 + 0.5 * xx / 20.0, 0.3 + 0.6 * yy / 20.0)
						var hit: Dictionary = w.call(&"pick", p)
						if StringName(hit.get("kind", &"")) == &"own" and int(hit.get("slot", -1)) >= 0:
							_mark["at"] = p
							_mark["slot"] = int(hit["slot"])
							break
					if Vector2(_mark["at"]).x >= 0.0:
						break
				if Vector2(_mark["at"]).x < 0.0:
					_fail("no part of the own hull under the view to right-click")
					_next_shot(0)
					return
				_mouse_button(_mark["at"], MOUSE_BUTTON_RIGHT, true)
				_phase = 1
				_wait = 3
			elif _phase == 1:
				_mouse_button(_mark["at"], MOUSE_BUTTON_RIGHT, false)
				_phase = 2
				_wait = 30
			else:
				print("u7_capture: right-clicked part ", _mark["slot"], " why open ", _why(hud).call(&"is_open"))
				if not bool(_why(hud).call(&"is_open")):
					_fail("right-clicking a part did not open Why?")
				_save(shot)
				_why(hud).call(&"close")
				_next_shot(0)
		"scroll_end":
			var sc: ScrollContainer = _control(hud).get_node(^"Scroll")
			sc.scroll_vertical = int(sc.get_v_scroll_bar().max_value)
			if _phase == 0:
				_phase = 1
				_wait = 5
			else:
				_save(shot + "_" + str(_shot))
				_next_shot(0)
		"close":
			if StringName(hud.call(&"station")) != &"":
				hud.call(&"close_station")
			_next_shot(10)
		_:
			if shot.begins_with("control_"):
				var page := StringName(shot.trim_prefix("control_"))
				if _phase == 0:
					if StringName(hud.call(&"station")) != &"control":
						hud.call(&"open_station", &"control")
					_control(hud).call(&"show_page", page)
					_phase = 1
					_wait = 30
				else:
					_save(shot)
					_next_shot(0)
			elif shot.begins_with("send_"):
				# Change a control through the panel's own handlers and check the order went out.
				_send(hud, shot.trim_prefix("send_"))
			else:
				_fail("unknown shot " + shot)
				_next_shot(0)


## Drive the control station's own controls and check the revert ids its chips use are the ones
## the server reports (`Hud.override_changed`), then revert each through its chip.
func _ids(hud: Node) -> void:
	var cp := _control(hud)
	match _phase:
		0:
			_mark["ids"] = PackedStringArray()
			Hud.override_changed.connect(_on_override_id)
			if StringName(hud.call(&"station")) != &"control":
				hud.call(&"open_station", &"control")
			cp.call(&"show_page", &"tuning")
			_wait = 10
		1:
			var rows: Dictionary = cp.get(&"_slider_rows")
			var row: Dictionary = rows.get("helm.evasion", {})
			if row.is_empty():
				_fail("no helm.evasion slider on the tuning page")
			else:
				var slider: HSlider = row["slider"]
				slider.drag_started.emit()
				slider.value = 0.9
				slider.drag_ended.emit(true)
			cp.call(&"show_page", &"doctrine")
			_wait = 10
		2:
			var mounts: Array = cp.get(&"_mount_rows")
			if mounts.is_empty():
				_fail("no mount rows on the doctrine page")
			else:
				var o: OptionButton = mounts[0]["option"]
				o.select(1)
				o.item_selected.emit(1)
				_mark["mount"] = int(mounts[0]["slot"])
			var drows: Array = cp.get(&"_doctrine_rows")
			if drows.is_empty():
				_fail("no doctrine rows on the doctrine page")
			else:
				var st: OptionButton = drows[0]["stance"]
				var to := 1 if st.selected == 0 else 0
				st.select(to)
				st.item_selected.emit(to)
			cp.call(&"show_page", &"power")
			_wait = 10
		3:
			var prow: Array = cp.get(&"_power_rows")
			if prow.is_empty():
				_fail("no power rows")
			else:
				var o: OptionButton = prow[0]["option"]
				var to := 0 if o.selected != 0 else 4
				o.select(to)
				o.item_selected.emit(to)
				_mark["power"] = int(prow[0]["slot"])
			_wait = 60
		4:
			var got: PackedStringArray = _mark["ids"]
			print("u7_capture: override ids ", got)
			var script: GDScript = cp.get_script()
			for want: String in [String(script.call(&"_override_id", "slider", "helm.evasion")), String(script.call(&"_override_id", "mount_preset", _mark.get("mount", -1))), String(script.call(&"_override_id", "power_priority", _mark.get("power", -1)))]:
				if not got.has(want):
					_fail("the server did not report override id " + want)
			var gun := 0
			for o: Dictionary in Hud.board()["officers"]:
				if int(o["role"]) == Bp.OFFICER_GUNNERY:
					gun = int(o["overrides"])
			_mark["gun"] = gun
			print("u7_capture: gunnery overrides after a doctrine change ", gun)
			if gun < 2:
				_fail("the mount preset and doctrine row did not count as Gunnery overrides (%d)" % gun)
			_save("ids_set")
			# Revert each through its chip.
			cp.call(&"show_page", &"tuning")
			_wait = 10
		5:
			var rows: Dictionary = cp.get(&"_slider_rows")
			if rows.has("helm.evasion"):
				(rows["helm.evasion"]["chip"] as Button).revert_requested.emit()
			cp.call(&"show_page", &"doctrine")
			_wait = 10
		6:
			var mounts: Array = cp.get(&"_mount_rows")
			if not mounts.is_empty():
				(mounts[0]["chip"] as Button).revert_requested.emit()
			var drows: Array = cp.get(&"_doctrine_rows")
			if not drows.is_empty():
				(drows[0]["chip"] as Button).revert_requested.emit()
			cp.call(&"show_page", &"power")
			_wait = 10
		7:
			var prow: Array = cp.get(&"_power_rows")
			if not prow.is_empty():
				(prow[0]["chip"] as Button).revert_requested.emit()
			_wait = 90
		_:
			var left := {}
			for o: Dictionary in Hud.board()["officers"]:
				left[int(o["role"])] = int(o["overrides"])
			print("u7_capture: overrides left after the chips ", left)
			for r: int in left:
				if int(left[r]) != 0:
					_fail("officer %d still has %d overrides after every chip reverted" % [r, left[r]])
			Hud.override_changed.disconnect(_on_override_id)
			_save("ids_reverted")
			_next_shot(0)
			return
	_phase += 1


func _on_override_id(id: String, overridden: bool) -> void:
	if overridden:
		var ids: PackedStringArray = _mark["ids"]
		ids.append(id)
		_mark["ids"] = ids


func _send(hud: Node, what: String) -> void:
	var cp := _control(hud)
	if _phase == 0:
		if _feed != null:
			_feed.call(&"take_commands")
		match what:
			"preset":
				cp.call(&"show_page", &"doctrine")
				cp.call(&"_on_ship_preset", Bp.PRESET_AGGRESSIVE)
			"emcon":
				cp.call(&"show_page", &"emissions")
				cp.call(&"_on_emcon", Bp.EMCON_SILENT)
			"rules":
				cp.call(&"show_page", &"rules")
				cp.call(&"_on_rule_toggled", false, 0)
			"priority":
				cp.call(&"show_page", &"power")
				var rows: Array = cp.get(&"_power_rows")
				if not rows.is_empty():
					cp.call(&"_on_priority", 4, int(rows[0]["slot"]))
		_phase = 1
		_wait = 30
	else:
		if _feed != null:
			var cmds: PackedStringArray = _feed.call(&"take_commands")
			print("u7_capture: commands after ", what, ": ", cmds)
		match what:
			"preset":
				if _feed == null and int(Hud.preset(Bp.NONE).get("preset", -1)) != Bp.PRESET_AGGRESSIVE:
					_fail("the ship preset did not change")
			"emcon":
				if _feed == null and int(Hud.emcon().get("emcon", -1)) != Bp.EMCON_SILENT:
					_fail("emission control did not change")
		_save("send_" + what)
		_next_shot(0)


static func Kit_dicts(v: Variant) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	if v is Array:
		for d: Variant in v:
			if d is Dictionary:
				out.append(d)
	return out


func _key(code: Key, pressed: bool, shift: bool = false) -> void:
	var e := InputEventKey.new()
	e.physical_keycode = code
	e.keycode = code
	e.pressed = pressed
	e.shift_pressed = shift
	Input.parse_input_event(e)


func _mouse_button(at_canvas: Vector2, button: MouseButton, pressed: bool) -> void:
	var window := get_viewport().get_final_transform() * at_canvas
	var motion := InputEventMouseMotion.new()
	motion.position = window
	motion.global_position = window
	Input.parse_input_event(motion)
	var e := InputEventMouseButton.new()
	e.button_index = button
	e.pressed = pressed
	e.position = window
	e.global_position = window
	if pressed:
		e.button_mask = MOUSE_BUTTON_MASK_RIGHT if button == MOUSE_BUTTON_RIGHT else MOUSE_BUTTON_MASK_LEFT
	Input.parse_input_event(e)


func _mouse(at_canvas: Vector2, pressed: bool) -> void:
	var window := get_viewport().get_final_transform() * at_canvas
	var motion := InputEventMouseMotion.new()
	motion.position = window
	motion.global_position = window
	Input.parse_input_event(motion)
	var e := InputEventMouseButton.new()
	e.button_index = MOUSE_BUTTON_LEFT
	e.pressed = pressed
	e.position = window
	e.global_position = window
	if pressed:
		e.button_mask = MOUSE_BUTTON_MASK_LEFT
	Input.parse_input_event(e)


func _save(name: String) -> void:
	var path := _out.path_join("%s_%s.png" % [_prefix, name])
	get_viewport().get_texture().get_image().save_png(path)
	print("u7_capture: saved ", path)


func _fail(text: String) -> void:
	_failures += 1
	push_error("u7_capture: " + text)


func _end() -> void:
	_active = false
	if _feed != null:
		_feed.call(&"stop")
	if _rules:
		DirAccess.remove_absolute(ProjectSettings.globalize_path(RULES_PATH))
	get_tree().quit(1 if _failures > 0 else 0)
