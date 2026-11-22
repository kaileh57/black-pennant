extends Node
## Plays the tutorial from a fresh install the way a new player would: the real boot scene, the consent
## screen, the main menu's first button, then every step of "The first ten minutes" (DESIGN.md L252-276)
## with simulated mouse and keyboard input only (Input.parse_input_event on the real controls), and real
## time passing. Saves a viewport capture at each step and prints what the player would see.
## Run with a throwaway HOME so the owner's saves and settings are never touched:
##   HOME=<scratch> godot --path game --resolution 1920x1080 res://test/ui/tutorial_play.tscn -- \
##       --out=<folder> [--scenario=full|wrong|leave|pause] [--tag=<prefix>] [--scale=2]
## Scenarios: `full` plays every step as asked; `wrong` also clicks controls the step does not allow,
## presses Escape on every step and opens the pause menu in the basin; `leave` leaves the dock in the
## middle and starts the tutorial again from the menu; `pause` pauses in each basin for a while.
## Exits 0 when the finish panel was reached and Return to main menu landed on the menu, 1 otherwise.
## Headless, or when instanced by check_project, it does nothing.

const BOOT := "res://boot/boot.tscn"
const STEP_LIMIT_S := 900.0

var driver := false
var _out := "user://captures"
var _tag := "play"
var _scenario := "full"
var _scale := 1.0
var _stop_after := ""
var _drag_kits := false
var _cheat_kits := false
var _level := -1
var _left := false
var _gun_last := false
var _shot := 0
var _t0 := 0.0
var _failed := false


## Counts the feed's simulation events each frame in a basin (diagnostics for the drone and patrol steps).
var _check: Object


func _process(_delta: float) -> void:
	if driver and _check != null and Session.state() == 3:
		_check.call(&"observe")


func _ready() -> void:
	if driver and ClassDB.class_exists(&"BpSessionCheck"):
		_check = ClassDB.instantiate(&"BpSessionCheck")
	if driver:
		process_mode = Node.PROCESS_MODE_ALWAYS
		_play.call_deferred()
		return
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	# Hand over to the real boot scene and keep a driver alive at the root.
	var d: Node = (get_script() as GDScript).new()
	d.set(&"driver", true)
	d.name = &"TutorialPlayDriver"
	get_tree().root.add_child.call_deferred(d)
	get_tree().change_scene_to_file.call_deferred(BOOT)


# --- the run ---------------------------------------------------------------------------------------

func _play() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--tag="):
			_tag = arg.trim_prefix("--tag=")
		elif arg.begins_with("--scenario="):
			_scenario = arg.trim_prefix("--scenario=")
		elif arg.begins_with("--scale="):
			_scale = float(arg.trim_prefix("--scale="))
		elif arg == "--drag-kits":
			_drag_kits = true
		elif arg == "--cheat-kits":
			_cheat_kits = true
		elif arg == "--gun-last":
			_gun_last = true
		elif arg.begins_with("--level="):
			_level = int(arg.trim_prefix("--level="))
		elif arg.begins_with("--stop-after="):
			_stop_after = arg.trim_prefix("--stop-after=")
	DirAccess.make_dir_recursive_absolute(_out)
	_t0 = Time.get_ticks_msec() / 1000.0
	var ok := await _run()
	_log("result %s" % ("pass" if ok and not _failed else "FAIL"))
	get_tree().quit(0 if ok and not _failed else 1)


func _run() -> bool:
	# Boot: consent, quality check, warm-up, main menu.
	if not await _until(func() -> bool: return _base() in [&"consent", &"main_menu"], 180.0, "consent or menu"):
		return false
	if _scale != 1.0:
		get_tree().root.content_scale_factor = _scale
	if _base() == &"consent":
		await _wait(1.0)
		_capture("consent")
		# At UI scale 200% the buttons sit below the fold: scroll to them as a player would.
		var dont := _find_button_path(Screens.top(), ^"Scroll/Center/Margin/Column/Buttons/DontSend")
		if dont != null:
			_scroll_into_view(dont)
			await get_tree().process_frame
			await get_tree().process_frame
		_click(dont)
	if not await _until(func() -> bool: return _base() == &"main_menu" and not _covered(), 240.0, "main menu"):
		return false
	await _wait(1.5)
	_capture("menu")
	if _level >= 0:
		# A fallback level (L270-274), as a playtest build would ship it.
		Settings.set_value(&"editor.tutorial_level", _level)
	var primary := Screens.top().get_node_or_null(^"Scroll/Margin/Column/Primary/PrimaryButton") as Button
	_log("menu primary: %s" % (primary.text if primary != null else "<none>"))
	_click(primary)
	if not await _until(func() -> bool: return _card_visible(), 60.0, "tutorial card"):
		return false
	await _wait(1.5)
	return await _steps()


func _steps() -> bool:
	var seen: Dictionary = {}
	var started := Time.get_ticks_msec() / 1000.0
	while true:
		if _finish_visible():
			await _wait(1.0)
			_capture("finish")
			_log("finish body: %s" % (_overlay().get_node(^"Finish/Scroll/Center/Panel/Margin/Column/Body") as Label).text)
			var log_text := String(Tutorial.log_text())
			_log("timing log:\n" + log_text)
			# A cooperative run reaches every step by the player's own action and stamps both gates.
			if _scenario != "leave" and (not log_text.contains("Timed out: none") or log_text.contains("first fight: not reached") or log_text.contains("first splash kill: not reached")):
				_fail("the run timed out a step or missed a gate")
			_click(_overlay().get_node(^"Finish/Scroll/Center/Panel/Margin/Column/Actions/Menu") as Button)
			if not await _until(func() -> bool: return _base() == &"main_menu" and not _covered(), 30.0, "menu after finish"):
				return false
			await _wait(1.5)
			_capture("menu_after_finish")
			var primary := Screens.top().get_node_or_null(^"Scroll/Margin/Column/Primary/PrimaryButton") as Button
			_log("menu primary after finish: %s; was_finished %s" % [primary.text if primary != null else "<none>", str(Tutorial.was_finished())])
			return true
		var id := _step_id()
		if id == "":
			if not await _until(func() -> bool: return _step_id() != "" or _finish_visible(), 30.0, "next step"):
				return false
			continue
		var n := int(seen.get(id, 0))
		seen[id] = n + 1
		if n > 2:
			_fail("step %s came back %d times" % [id, n])
			return false
		_log("step %s: %s" % [id, _card_text()])
		_capture("%s" % id)
		if _scenario == "wrong":
			await _misbehave(id)
		if _scenario == "leave" and id == "kits" and not _left:
			_left = true
			if not await _leave_and_restart():
				return false
			seen.clear()
			continue
		var before := id
		await _do_step(id)
		if id == _stop_after:
			await _wait(1.0)
			_capture("%s_stop" % id)
			_log("stopping after %s; step now %s" % [id, _step_id()])
			return not _failed
		if not await _until(func() -> bool: return _step_id() != before or _finish_visible(), STEP_LIMIT_S, "step %s to end" % id):
			_capture("%s_stuck" % id)
			return false
		_log("step %s ended after %.1f s" % [before, Time.get_ticks_msec() / 1000.0 - started])
		await _wait(0.8)
		_capture("%s_done" % before)
	return false


func _do_step(id: String) -> void:
	match id:
		"open", "done":
			await _wait(1.0)
			_click(_card_button(^"Continue"))
		"beam":
			await _drag_beam()
		"kits":
			var order: Array[String] = ["kit.gun", "kit.radar_mast", "kit.engine", "kit.fuel_tank"]
			if _gun_last:
				order = ["kit.fuel_tank", "kit.engine", "kit.radar_mast", "kit.gun"]
			for kit: String in order:
				if _step_id() != "kits":
					break
				if _cheat_kits:
					_api_place(kit)
				else:
					await _drop_kit(kit)
				await _wait(1.0)
				_capture("kits_%s" % kit.trim_prefix("kit."))
		"fix_arc":
			await _apply_fix()
		"launch", "patrol_launch", "splash_again":
			await _splash()
		"drones":
			await _sail(true)
		"patrol_boat":
			await _sail(false)
		"aggression":
			await _move_aggression()
		_:
			_log("no action for step %s; waiting" % id)


# --- steps ---------------------------------------------------------------------------------------

func _drag_beam() -> void:
	var dock := _dock()
	await _wait(1.0)
	var r: Rect2 = dock.call(&"control_rect", "editor.handle.beam", int(_overlay().get(&"_point_station")))
	if not r.has_area():
		_fail("no beam handle on screen")
		return
	var from := r.get_center()
	_log("beam handle at %s" % from)
	_mouse_move(from)
	await _wait(0.3)
	_mouse_button(from, true)
	await _wait(0.1)
	_log("beam press: dragging %s" % str(dock.get(&"_handle_drag")))
	var at := from
	# Right, as the step says, while the view has room; at UI scale 200% the view is narrow, so the drag
	# carries on upward ("up or to the right") instead of leaving the view.
	var view: Rect2 = _overlay().call(&"_host_rect", ^"Scroll/Frame/Body/Centre/Stage")
	for i: int in 60:
		if not view.has_area() or at.x + 6.0 <= view.end.x - 12.0:
			at += Vector2(6.0, 0.0)
		elif at.y - 6.0 >= view.position.y + 12.0:
			at += Vector2(0.0, -6.0)
		else:
			break
		_mouse_move(at, true)
		await _wait(0.05)
		if _gauge_check("float") == "pass":
			break
	await _wait(0.2)
	_mouse_button(at, false)
	_log("beam drag ended at %s, float %s" % [at, _gauge_check("float")])
	await _wait(1.0)


func _gauge_check(name: String) -> String:
	var g: Variant = Editor.gauges()
	if g is Dictionary:
		var f: Variant = (g as Dictionary).get(name, {})
		if f is Dictionary:
			var c: Variant = (f as Dictionary).get("check")
			return {0: "pass", 1: "caution", 2: "fail"}.get(int(c), str(c)) if c is int else str(c)
	return "?"


## Drag a kit's tray row onto the hull: find the row, press it, move into the view, hover until the ghost
## is valid, release.
func _drop_kit(kit: String) -> void:
	var dock := _dock()
	var row := _find_meta(dock, "editor.tray." + kit)
	if row == null:
		_fail("no tray row for %s" % kit)
		return
	_scroll_into_view(row)
	await _wait(0.3)
	var start := row.get_global_rect().get_center()
	var display := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Display") as Control
	var area := display.get_global_rect()
	var mid := area.get_center()
	var p := start
	if _drag_kits:
		# Press on the row and drag it into the view.
		_mouse_move(start)
		await _wait(0.2)
		_mouse_button(start, true)
		await _wait(0.1)
		for i: int in 12:
			p = p.lerp(mid, 0.25)
			_mouse_move(p, true)
			await _wait(0.03)
	else:
		# Click the row (the tray's own hint: "Click to place it in the view or drag it there").
		_click(row)
		await _wait(0.3)
		for i: int in 8:
			p = p.lerp(mid, 0.3)
			_mouse_move(p)
			await _wait(0.03)
	_log("%s: ghost after pick %s dragging %s" % [kit, str(Editor.ghost()).left(120), str(get_viewport().gui_is_dragging())])
	# Hover over the hull until the dock reports a valid place (a player watching the ghost turn green).
	var spot := await _hover_valid(area, _drag_kits)
	if spot == Vector2.INF:
		if _drag_kits:
			_mouse_button(p, false)
		_fail("no valid spot for %s" % kit)
		await _wait(0.3)
		_key(KEY_ESCAPE)
		return
	if _drag_kits:
		_mouse_button(spot, false)
	else:
		_click_at(spot)
	await _wait(0.4)
	var count := _kit_count(kit)
	if Editor.ghost() is Dictionary:
		# Placing goes on until Esc.
		_key(KEY_ESCAPE)
	else:
		# A second click where the kit landed must not drop a copy.
		_click_at(spot)
		await _wait(0.4)
		if _kit_count(kit) != count:
			_fail("a second click placed another %s" % kit)
	_log("%s placed: %s" % [kit, str(count)])


## Reconnaissance only (`--cheat-kits`): place a kit through the Editor API near a known good cell.
func _api_place(kit: String) -> void:
	var c := int((Editor.hull() as Dictionary).get("size_cells", Vector3i(44, 14, 12)).z) / 2
	var want: Vector3i = {"kit.gun": Vector3i(35, 4, c), "kit.radar_mast": Vector3i(21, 14, c), "kit.engine": Vector3i(17, 3, c + 2), "kit.fuel_tank": Vector3i(25, 2, c + 2)}[kit]
	Editor.pick_kit(kit)
	var cands: Array[Vector3i] = []
	for dx: int in range(-10, 11):
		for dy: int in range(-6, 7):
			for dz: int in range(-5, 6):
				cands.append(want + Vector3i(dx, dy, dz))
	cands.sort_custom(func(a: Vector3i, b: Vector3i) -> bool: return (a - want).length_squared() < (b - want).length_squared())
	for cell: Vector3i in cands:
		if bool((Editor.ghost_move(cell) as Dictionary).get("valid", false)):
			_log("api place %s at %s: %s" % [kit, cell, str(Editor.ghost_place())])
			break
	Editor.ghost_cancel()


func _kit_count(kit: String) -> int:
	var n := 0
	for k: Dictionary in Editor.kits():
		if String(k.get("kit", "")).begins_with(kit):
			n += 1
	return n


## Sweep the view in a grid, centre outward, until the ghost is valid. Returns the spot or INF.
func _hover_valid(area: Rect2, dragging: bool) -> Vector2:
	var c := area.get_center()
	var spots: Array[Vector2] = []
	for gy: int in range(-6, 7):
		for gx: int in range(-10, 11):
			spots.append(c + Vector2(gx * area.size.x / 26.0, gy * area.size.y / 16.0))
	spots.sort_custom(func(a: Vector2, b: Vector2) -> bool: return a.distance_squared_to(c) < b.distance_squared_to(c))
	var view: Node = _dock().get(&"_view")
	var k := 0
	for s: Vector2 in spots:
		_mouse_move(s, dragging)
		await _wait(0.03)
		var g: Variant = Editor.ghost()
		if k % 40 == 0:
			var hovered := get_viewport().gui_get_hovered_control()
			var local := s - (_dock().get_node(^"Scroll/Frame/Body/Centre/Stage/Display") as Control).get_global_rect().position
			_log("  sweep %s hovered %s cell %s ghost %s dragging %s" % [s, hovered.name if hovered != null else "-", str(view.call(&"cell_at", local)) if view != null else "?", str(g).left(160), str(get_viewport().gui_is_dragging())])
		k += 1
		if g is Dictionary and bool((g as Dictionary).get("valid", false)):
			return s
	return Vector2.INF


func _apply_fix() -> void:
	await _wait(1.5)
	var dock := _dock()
	var fix := _find_meta(dock, "editor.notes.fix.arc_blocked")
	if fix == null:
		_log("no arc Fix on screen; notes: %s" % str(Editor.notes()).left(400))
		return
	_click(fix)
	await _wait(1.0)
	_capture("fix_pressed")
	# The Fix previews; Accept fix applies it.
	if _step_id() == "fix_arc":
		var accept := _find_meta(dock, "editor.notes.fix.arc_blocked")
		if accept != null:
			_click(accept)


func _splash() -> void:
	await _wait(1.0)
	# Space, as the card says, in the dry dock or still in the last basin.
	_key(KEY_SPACE)
	await _wait(0.5)


## The step is played in the dry dock while the basin is open: use the card's Return to dry dock, then read
## the splash report and close it, as a player would.
func _back_to_dock() -> void:
	if Editor.in_splash():
		var back := _card_button(^"Back")
		if back != null and back.visible:
			_log("card offers: %s" % back.text)
			_click(back)
			await _until(func() -> bool: return not Editor.in_splash(), 30.0, "back in the dock")
			await _wait(2.0)
			_capture("%s_back_in_dock" % _step_id())
		else:
			_fail("in the basin with no way back on the card")
	var report := _dock().get_node_or_null(^"ReportLayer/Report") as Control
	if report != null and report.visible:
		await _wait(1.0)
		_capture("%s_report" % _step_id())
		for b: Node in report.find_children("*", "Button", true, false):
			var t := (b as Button).text
			if t == "editor_splash.report.back" or t == tr(&"editor_splash.report.back"):
				_click(b as Button)
				await _wait(0.8)
				break


## Sail in the basin: helm with WASD, watching. `drones` waits for the guns; the patrol boat waits for a kill.
func _sail(drones: bool) -> void:
	if not await _until(func() -> bool: return Session.state() == 3 and not _covered(), 60.0, "basin"):
		return
	await _wait(3.0)
	_capture("%s_basin" % _step_id())
	var i := 0
	var step := _step_id()
	var t := 0.0
	# Ring the engines up as the card asks: W steps the telegraph a quarter per press. Then steer with A and
	# D toward the nearest hostile.
	for _n in 4:
		_key(KEY_W)
		await _wait(0.3)
	await _wait(2.0)
	_log("%s: after W ordered %s" % [step, str((Hud.motion() as Dictionary).get("ordered_speed_kn", "?"))])
	while _step_id() == step and t < STEP_LIMIT_S:
		var turn := _turn_toward_hostile()
		if turn != 0:
			var k := KEY_D if turn > 0 else KEY_A
			_key_hold(k, true)
			await _wait(0.8)
			_key_hold(k, false)
			await _wait(1.2)
		else:
			await _wait(2.0)
		i += 1
		t += 2.0
		if i % 5 == 0:
			_log("%s: t=%.0f %s" % [step, t, _helm_line()])
		if i % 10 == 0:
			_capture("%s_t%03d" % [step, int(t)])
		if _scenario == "pause" and i == 4:
			await _pause_for(20.0)


## One line of what the bridge shows: who has the helm, ordered and actual speed, nearest hostile, own shots.
func _helm_line() -> String:
	var m := Hud.motion() as Dictionary
	var holder: Variant = Hud.role_holder(Bp.ROLE_HELM, 0)
	var near := -1.0
	for tr: Dictionary in Hud.tracks():
		if int(tr.get("stance", -1)) == 3:
			var r := float(tr.get("range_m", -1.0))
			if r > 0.0 and (near < 0.0 or r < near):
				near = r
	var shots: Variant = "?"
	if _check != null:
		shots = (_check.call(&"facts") as Dictionary).get("own_mount_fired")
	var holds := []
	for mount: Dictionary in Hud.mounts():
		var reason := String((mount.get("reason", {}) as Dictionary).get("key", ""))
		holds.append("%s:%s" % [str(mount.get("slot")), reason.get_slice(".", reason.get_slice_count(".") - 1)])
	return "helm %s ordered %.1f speed %.1f hostile %.0f m own shots %s mounts %s" % [str(holder), float(m.get("ordered_speed_kn", 0.0)), float(m.get("speed_kn", 0.0)), near, str(shots), " ".join(holds)]


## +1 to turn right, -1 left, 0 when the nearest hostile is within 15 degrees of the bow (or none is known).
func _turn_toward_hostile() -> int:
	var heading := float((Hud.motion() as Dictionary).get("heading_deg", 0.0))
	var best := -1.0
	var bearing := 0.0
	for tr: Dictionary in Hud.tracks():
		if int(tr.get("stance", -1)) != 3:
			continue
		var r := float(tr.get("range_m", -1.0))
		if r > 0.0 and (best < 0.0 or r < best):
			best = r
			bearing = float(tr.get("bearing_deg", 0.0))
	if best < 0.0:
		return 0
	var off := fmod(bearing - heading + 540.0, 360.0) - 180.0
	return 0 if absf(off) < 15.0 else (1 if off > 0.0 else -1)


func _move_aggression() -> void:
	await _wait(1.0)
	await _back_to_dock()
	var host := _dock()
	var s := _find_meta(host, "editor.officers.slider.gunnery.aggression")
	if s == null:
		_fail("no Aggression slider on screen")
		return
	_scroll_into_view(s)
	await _wait(0.3)
	var bar: Control = s
	for c: Node in s.find_children("*", "HSlider", true, false):
		bar = c as Control
	var r := bar.get_global_rect()
	var from := Vector2(r.position.x + r.size.x * 0.5, r.get_center().y)
	_log("aggression slider at %s" % str(r))
	_mouse_move(from)
	_mouse_button(from, true)
	for k: int in 10:
		_mouse_move(from + Vector2(k * 8.0, 0.0), true)
		await _wait(0.05)
	_mouse_button(from + Vector2(80.0, 0.0), false)
	await _wait(1.0)


# --- wrong things --------------------------------------------------------------------------------

func _misbehave(id: String) -> void:
	await _wait(0.5)
	# A control the step does not allow (Save in the dry dock), then Escape.
	if not Editor.in_splash():
		var save := _dock().get_node_or_null(^"Scroll/Frame/TopBar/Margin/Row/Save") as Button
		if save != null:
			_click(save)
			await _wait(0.4)
			_capture("%s_wrong_click" % id)
	_key(KEY_ESCAPE)
	await _wait(0.8)
	_capture("%s_escape" % id)
	_log("after Esc on %s: top %s modal %s step %s" % [id, str(Screens.top().name), str(_covered()), _step_id()])
	# Close whatever Esc opened.
	if _covered() or Screens.top() != _dock():
		_key(KEY_ESCAPE)
		await _wait(0.8)


## Leave the dry dock in the middle of the tutorial, answer its question, and start it again from the menu.
func _leave_and_restart() -> bool:
	_click(_dock().get_node(^"Scroll/Frame/TopBar/Margin/Row/Leave") as Button)
	await _wait(1.0)
	_capture("leave_question")
	var dialog := _top_dialog()
	if dialog == null:
		_fail("Leave dry dock asked nothing")
		return false
	for b: Node in dialog.find_children("*", "Button", true, false):
		if (b as Button).text == tr(&"tutorial_overlay.skip.confirm") or (b as Button).text == "tutorial_overlay.skip.confirm":
			_click(b as Button)
			break
	if not await _until(func() -> bool: return _base() == &"main_menu" and not _covered(), 30.0, "menu after leaving"):
		return false
	await _wait(1.5)
	_capture("menu_after_leave")
	var primary := Screens.top().get_node_or_null(^"Scroll/Margin/Column/Primary/PrimaryButton") as Button
	_log("menu primary after leaving: %s; tutorial state %d" % [primary.text if primary != null else "<none>", Tutorial.state()])
	_click(primary)
	return await _until(func() -> bool: return _card_visible(), 60.0, "tutorial card again")


func _top_dialog() -> Node:
	var layer: Variant = Screens.get(&"_modal_layer")
	if layer is Node and (layer as Node).get_child_count() > 0:
		return (layer as Node).get_child((layer as Node).get_child_count() - 1)
	return null


func _pause_for(seconds: float) -> void:
	# P is the pause key (Escape in a splash belongs to the dry dock under it).
	_key(KEY_P)
	await _wait(1.0)
	_capture("%s_paused" % _step_id())
	_log("%s: paused %s" % [_step_id(), str(Session.call(&"pacing") if Session.has_method(&"pacing") else "?")])
	await _wait(seconds)
	_key(KEY_P)
	await _wait(1.0)


# --- helpers: what is on screen ------------------------------------------------------------------

func _base() -> StringName:
	return Screens.base_name()


func _covered() -> bool:
	var cover: Variant = Screens.get(&"_cover")
	if cover is CanvasItem and is_instance_valid(cover) and (cover as CanvasItem).visible:
		return true
	return false


func _dock() -> Control:
	var top := Screens.top()
	var n: Node = top
	while n != null and not n.has_method(&"control_rect"):
		n = n.get_parent()
	if n == null:
		for c: Node in get_tree().root.get_children():
			if c.has_method(&"control_rect"):
				return c as Control
	return n as Control


func _overlay() -> Node:
	var d := _dock()
	return d.get_node_or_null(^"TutorialOverlay") if d != null else null


func _card_visible() -> bool:
	var o := _overlay()
	return o != null and (o.get_node(^"Card") as Control).visible


func _finish_visible() -> bool:
	var o := _overlay()
	return o != null and (o.get_node(^"Finish") as Control).visible


func _card_text() -> String:
	var o := _overlay()
	if o == null:
		return ""
	var t := (o.get_node(^"Card/Margin/Column/Text") as Label).text
	var p := (o.get_node(^"Card/Margin/Column/Footer/Progress") as Label).text
	var buttons: PackedStringArray = []
	for b: Button in [_card_button(^"Skip"), _card_button(^"Back"), _card_button(^"Continue")]:
		if b != null and b.visible:
			buttons.append(b.text)
	return "%s [%s] {%s} pointer %s" % [t, p, ", ".join(buttons), str(o.get(&"_target_rect"))]


func _card_button(name: NodePath) -> Button:
	var o := _overlay()
	return o.get_node_or_null(NodePath("Card/Margin/Column/Footer/" + String(name))) as Button if o != null else null


func _step_id() -> String:
	var s: Variant = Tutorial.step()
	return String((s as Dictionary).get("id", "")) if s is Dictionary else ""


func _find_button_path(root: Node, path: NodePath) -> Control:
	return root.get_node_or_null(path) as Control if root != null else null


func _find_meta(node: Node, id: String) -> Control:
	if node is CanvasItem and not (node as CanvasItem).is_visible_in_tree():
		return null
	if node is Control and node.has_meta(&"control_id") and String(node.get_meta(&"control_id")) == id:
		return node as Control
	for c: Node in node.get_children():
		var f := _find_meta(c, id)
		if f != null:
			return f
	return null


func _find_meta_text(node: Node, id: String) -> Control:
	return _find_meta(node, id)


func _scroll_into_view(c: Control) -> void:
	var p := c.get_parent()
	while p != null:
		if p is ScrollContainer:
			(p as ScrollContainer).ensure_control_visible(c)
			return
		p = p.get_parent()


# --- helpers: input ------------------------------------------------------------------------------

## Canvas position to window position, through the root's stretch transform.
func _win(p: Vector2) -> Vector2:
	return get_viewport().get_final_transform() * p


var _mouse_at := Vector2.ZERO


func _mouse_move(p: Vector2, held: bool = false) -> void:
	var e := InputEventMouseMotion.new()
	e.position = _win(p)
	e.global_position = e.position
	e.relative = e.position - _mouse_at
	e.screen_relative = e.relative
	_mouse_at = e.position
	if held:
		e.button_mask = MOUSE_BUTTON_MASK_LEFT
	Input.parse_input_event(e)


func _mouse_button(p: Vector2, pressed: bool, button: MouseButton = MOUSE_BUTTON_LEFT) -> void:
	var e := InputEventMouseButton.new()
	e.position = _win(p)
	e.global_position = e.position
	e.button_index = button
	e.pressed = pressed
	if pressed:
		e.button_mask = MOUSE_BUTTON_MASK_LEFT
	Input.parse_input_event(e)


func _click(c: Control) -> void:
	if c == null:
		_fail("click on a missing control")
		return
	_log("click %s '%s'%s" % [c.name, c.get(&"text") if c.get(&"text") != null else "", " (disabled)" if c is BaseButton and (c as BaseButton).disabled else ""])
	_click_at(c.get_global_rect().get_center())


func _click_at(p: Vector2) -> void:
	_mouse_move(p)
	_mouse_button(p, true)
	_mouse_button(p, false)


func _key(code: Key) -> void:
	_key_hold(code, true)
	_key_hold(code, false)


func _key_hold(code: Key, pressed: bool) -> void:
	var e := InputEventKey.new()
	e.keycode = code
	e.physical_keycode = code
	e.pressed = pressed
	Input.parse_input_event(e)


# --- helpers: time and output --------------------------------------------------------------------

func _wait(seconds: float) -> void:
	await get_tree().create_timer(seconds, true, false, true).timeout


func _until(cond: Callable, limit: float, what: String) -> bool:
	var t := 0.0
	while not bool(cond.call()):
		await _wait(0.1)
		t += 0.1
		if t > limit:
			_fail("waited %.0f s for %s" % [limit, what])
			return false
	return true


func _capture(name: String) -> void:
	_shot += 1
	var path := _out.path_join("%s_%02d_%s.png" % [_tag, _shot, name])
	get_viewport().get_texture().get_image().save_png(path)
	_log("capture %s" % path)
	_check_basin_card()


## In the basin the card must sit clear of every HUD cluster and the splash bar, and should cover as little of
## the own ship as the screen allows: report what it covers, and fail a frame where it covers a cluster.
func _check_basin_card() -> void:
	var o := _overlay()
	if o == null:
		return
	if not bool(o.call(&"_in_basin")):
		var c := o.get_node_or_null(^"Card") as Control
		if c != null and c.visible and _scale != 1.0:
			var screen := get_viewport().get_visible_rect()
			_log("  dock card %s; view below the fold %s; gauges %s drawn %s; view %s" % [c.get_global_rect(), str(o.call(&"_view_below_fold", screen)),
				o.call(&"_host_rect", ^"Scroll/Frame/Gauges"), o.call(&"_drawn_rect", ^"Scroll/Frame/Gauges", 4), o.call(&"_host_rect", ^"Scroll/Frame/Body/Centre/Stage")])
		return
	var card := o.get_node_or_null(^"Card") as Control
	if card == null or not card.visible:
		return
	var r := card.get_global_rect()
	var ship: Rect2 = o.call(&"_own_ship_rect")
	var covered := r.intersection(ship).get_area() / maxf(ship.get_area(), 1.0) if ship.has_area() else 0.0
	var report: Dictionary = o.call(&"basin_report")
	_log("  basin card %s ship %s covers %d%% of the ship; placement cost %s" % [r, ship, roundi(covered * 100.0), str(report.get("cost", "?"))])
	var hits: Array[Rect2] = []
	for h: Rect2 in o.call(&"_basin_hard_rects"):
		if r.intersects(h):
			hits.append(h)
	if hits.is_empty():
		return
	# A cluster that grew this frame moves the card on the next one: look again a few frames later.
	for i in 3:
		await get_tree().process_frame
	if not is_instance_valid(card) or not card.visible or not bool(o.call(&"_in_basin")):
		return
	r = card.get_global_rect()
	report = o.call(&"basin_report")
	for h: Rect2 in o.call(&"_basin_hard_rects"):
		if r.intersects(h):
			# Placed over a cluster only when no clear spot was left (the placement's cost says so); a card over
			# a cluster with a clear spot on screen is a failure.
			var forced := float(report.get("hard_cover", 0.0)) >= 1.0
			_log("  basin card covers a HUD cluster at %s%s" % [h, " (no clear spot on screen)" if forced else ""])
			if not forced:
				_fail("the basin card %s covers a HUD cluster or the splash bar at %s" % [r, h])


func _log(text: String) -> void:
	print("[play %6.1f] %s" % [Time.get_ticks_msec() / 1000.0 - _t0, text])


func _fail(text: String) -> void:
	_failed = true
	push_error("tutorial_play: " + text)
	_log("FAIL " + text)
