extends Node
## Windowed capture of the tutorial overlay over the dry dock, driven through the real `Tutorial` facade.
## Unit ui-tutorial. Run from the repository root with a throwaway HOME (a finished run writes settings and
## blueprints under user://):
##   HOME=/tmp/bp-tut godot --path game --resolution 1600x900 res://test/ui/tutorial_capture.tscn -- \
##       --state=<state> --out=<folder>
## States: open, beam, beam_demo, beam_after, fix_demo, kits, gun, launch, splash, splash_long, splash_back, splash_back_go, splash_again, skip_basin, soak4, beam_widen, beam_widen_default, finish, finish_again, finish_open, skip, skip_go, blocked; any state with
## _hc (high contrast) or _s200 (UI scale 200%) appended. Writes <folder>/tutorial_<state>.png and quits; exits 1
## on a failed setup. Prints the step on screen, the pointer rectangle and the card rectangle, and for
## beam_demo the handle value and undo state before, during and after the demonstration.

const SKY := """
shader_type sky;
uniform vec3 zenith;
uniform vec3 horizon;
uniform vec3 sun_col;
void sky() {
	float h = clamp(EYEDIR.y, -1.0, 1.0);
	vec3 c = mix(horizon, zenith, pow(clamp(h, 0.0, 1.0), 0.45));
	if (h < 0.0) { c = horizon * mix(0.6, 0.25, clamp(-h * 4.0, 0.0, 1.0)); }
	float sd = dot(EYEDIR, LIGHT0_DIRECTION);
	c += sun_col * (pow(max(sd, 0.0), 6.0) * 0.6 + smoothstep(0.9995, 0.9998, sd) * 30.0);
	COLOR = c;
}
"""

var _state := "open"
var _base := "open"
var _out := "user://captures"
var _frame := 0
var _dock: Control
var _overlay: Node
var _shot_at := -1
var _plan: Array[Dictionary] = []
var _beam_before := ""
var _shot_done := false
var _soak := false
var _soak_phase := 0
var _soak_at := 0


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--state="):
			_state = arg.trim_prefix("--state=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
	_base = _state.replace("_hc", "").replace("_s200", "")
	DirAccess.make_dir_recursive_absolute(_out)
	var settings := get_node_or_null(^"/root/Settings")
	if settings != null and settings.has_method(&"reload"):
		settings.call(&"reload")
	Ui.refresh()
	if _state.contains("_hc"):
		Settings.set_value(&"access.palette", 1)
		Ui.call(&"_apply_palette", &"high_contrast")
	if _state.contains("_s200"):
		get_tree().root.content_scale_factor = 2.0


func _process(_delta: float) -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	_frame += 1
	if _frame == 2 and not _setup():
		get_tree().quit(1)
		return
	if _soak:
		_soak_tick()
	if _frame > (40000 if _soak else 6000):
		push_error("tutorial_capture: timed out")
		get_tree().quit(1)
		return
	while not _plan.is_empty() and int(_plan[0]["at"]) <= _frame:
		var step: Dictionary = _plan.pop_front()
		(step["do"] as Callable).call()
	if _frame == _shot_at:
		_report()
		var path := _out.path_join("tutorial_%s.png" % _state)
		get_viewport().get_texture().get_image().save_png(path)
		print("tutorial_capture: saved ", path)
		_shot_done = true
	if _shot_done and _plan.is_empty():
		Tutorial.stop()
		Editor.close()
		get_tree().quit(0)


## The level 4 soak: wait in the drone basin until the step escapes to Done, press Finish tutorial on the
## card, check the finish panel and the reward, then Return to main menu.
func _soak_tick() -> void:
	if _frame % 600 == 0:
		print("tutorial_capture: soak t+", _frame / 60, " s step ", _step_id(), " tutorial ", Tutorial.state(), " in splash ", Editor.in_splash())
	match _soak_phase:
		0:
			if _step_id() == "done":
				_soak_phase = 1
				_soak_at = _frame + 60
		1:
			if _frame == _soak_at:
				var c := _overlay.get_node(^"Card/Margin/Column/Footer/Continue") as Button
				print("tutorial_capture: done step continue visible ", c.visible, " text ", c.text)
				c.pressed.emit()
				_soak_phase = 2
				_soak_at = _frame + 60
		2:
			if _frame == _soak_at:
				var stay := _overlay.get_node(^"Finish/Scroll/Center/Panel/Margin/Column/Actions/Row/Stay") as Button
				print("tutorial_capture: finish visible ", (_overlay.get_node(^"Finish") as Control).visible, " stay ", stay.text, " tutorial ", Tutorial.state(), " was finished ", Tutorial.was_finished())
				print("tutorial_capture: body ", (_overlay.get_node(^"Finish/Scroll/Center/Panel/Margin/Column/Body") as Label).text)
				print("tutorial_capture: blueprints ", Editor.browse("").map(func(e: Dictionary) -> String: return String(e.get("path", ""))))
				_report()
				var path := _out.path_join("tutorial_%s.png" % _state)
				get_viewport().get_texture().get_image().save_png(path)
				print("tutorial_capture: saved ", path)
				(_overlay.get_node(^"Finish/Scroll/Center/Panel/Margin/Column/Actions/Menu") as Button).pressed.emit()
				_soak_phase = 3
				_soak_at = _frame + 60
		3:
			if _frame == _soak_at:
				print("tutorial_capture: after menu screen ", Screens.base_name(), " session ", Session.state(), " in splash ", Editor.in_splash(), " dock open ", Editor.is_open())
				get_tree().quit(0)


func _at(frame: int, action: Callable) -> void:
	_plan.append({"at": frame, "do": action})


func _setup() -> bool:
	var level := 0
	match _base:
		"beam_demo", "beam_after":
			level = 1
		"kits":
			level = 2
		"gun", "launch", "splash", "splash_long", "splash_back", "splash_back_go", "splash_again", "skip_basin", "soak4":
			level = 4
		"fix_demo":
			level = 3
	if not Tutorial.start(level):
		push_error("tutorial_capture: Tutorial.start(%d) refused" % level)
		return false
	Screens.change(&"dry_dock")
	_dock = Screens.top() as Control
	if _dock == null:
		push_error("tutorial_capture: no dock screen")
		return false
	_overlay = _dock.get_node_or_null(^"TutorialOverlay")
	if _overlay == null:
		push_error("tutorial_capture: the dock did not mount the overlay")
		return false
	_light(_dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport") as SubViewport)
	var f := 120
	match _base:
		"open":
			_shot_at = f
		"beam":
			_at(f, func() -> void: Tutorial.continue_step())
			_shot_at = f + 90
		"blocked":
			# A click on a tray row during the text step: the overlay frames the card.
			_at(f, func() -> void: _click_first_tray_row())
			_shot_at = f + 20
		"beam_demo", "beam_after":
			_at(f, func() -> void:
				_beam_before = _beam_state()
				print("tutorial_capture: before demo ", _beam_before)
				Tutorial.continue_step())
			_at(f + 360, func() -> void: print("tutorial_capture: during demo ", _beam_state()))
			_at(f + 60 * 20, func() -> void:
				var after := _beam_state()
				print("tutorial_capture: after demo ", after, " step ", _step_id())
				if after != _beam_before:
					push_error("tutorial_capture: the beam demonstration changed the design"))
			_shot_at = f + (360 if _base == "beam_demo" else 60 * 20 + 30)
		"kits":
			_at(f, func() -> void: Tutorial.continue_step())
			_at(f + 30, func() -> void: _overlay.call(&"_on_continue"))
			_shot_at = f + 120
		"fix_demo":
			_at(f, func() -> void: Tutorial.continue_step())
			_at(f + 30, func() -> void: _overlay.call(&"_on_continue"))
			_at(f + 60, func() -> void:
				print("tutorial_capture: gun placed ", _place_kit(&"kit.gun"))
				print("tutorial_capture: mast placed ", _place_kit(&"kit.radar_mast")))
			_at(f + 90, func() -> void: print("tutorial_capture: notes ", Editor.notes()))
			_shot_at = f + 400
		"gun":
			_at(f, func() -> void: Tutorial.continue_step())
			_shot_at = f + 90
		"launch":
			_at(f, func() -> void: Tutorial.continue_step())
			_at(f + 30, func() -> void: print("tutorial_capture: gun placed ", _place_kit(&"kit.gun")))
			_shot_at = f + 120
		"beam_widen", "beam_widen_default":
			# Widen the framed handle (or the dock's default station) step by step, as a player dragging it would,
			# until Float passes and the step ends.
			_at(f, func() -> void: Tutorial.continue_step())
			for k: int in range(0, 60):
				_at(f + 60 + k * 12, func() -> void: _widen_once())
			_shot_at = f + 60 + 60 * 12 + 30
		"skip_basin":
			_at(f, func() -> void: Tutorial.continue_step())
			_at(f + 30, func() -> void: print("tutorial_capture: gun placed ", _place_kit(&"kit.gun")))
			_at(f + 90, func() -> void: (_dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/Splash") as Button).pressed.emit())
			_at(f + 600, func() -> void:
				print("tutorial_capture: in splash ", Editor.in_splash(), " session ", Session.state())
				_overlay.call(&"_on_skip_chosen", &"skip"))
			_at(f + 660, func() -> void: print("tutorial_capture: after skip screen ", Screens.base_name(), " tutorial ", Tutorial.state(), " in splash ", Editor.in_splash(), " dock open ", Editor.is_open(), " session ", Session.state()))
			_shot_at = f + 680
		"soak4":
			_at(f, func() -> void: Tutorial.continue_step())
			_at(f + 30, func() -> void: print("tutorial_capture: gun placed ", _place_kit(&"kit.gun")))
			_at(f + 90, func() -> void: (_dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/Splash") as Button).pressed.emit())
			_soak = true
			_shot_at = -1
		"splash", "splash_long", "splash_back", "splash_back_go", "splash_again":
			_at(f, func() -> void: Tutorial.continue_step())
			_at(f + 30, func() -> void: print("tutorial_capture: gun placed ", _place_kit(&"kit.gun")))
			_at(f + 90, func() -> void:
				var b := _dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/Splash") as Button
				print("tutorial_capture: splash button disabled ", b.disabled)
				b.pressed.emit())
			_shot_at = f + (900 if _base == "splash" else 4200)
			if _base.begins_with("splash_back") or _base == "splash_again":
				# Level 4 has no second splash step: hand the card one that asks for a splash, as patrol_launch
				# does while the drone basin is still open, and press its Return to dry dock.
				_at(f + 600, func() -> void:
					_overlay.set(&"_target_id", "editor.launch")
					_overlay.call(&"_refresh_back")
					print("tutorial_capture: back offered ", (_overlay.get_node(^"Card/Margin/Column/Footer/Back") as Control).visible, " in splash ", Editor.in_splash()))
				_at(f + 660, func() -> void: (_overlay.get_node(^"Card/Margin/Column/Footer/Back") as Button).pressed.emit())
				_at(f + 840, func() -> void: print("tutorial_capture: after back in splash ", Editor.in_splash(), " session ", Session.state(), " back offered ", (_overlay.get_node(^"Card/Margin/Column/Footer/Back") as Control).visible))
				_shot_at = f + (630 if _base == "splash_back" else 880)
				if _base == "splash_again":
					# Back in the dock on a basin step (the drones step points at nothing): Splash again.
					_at(f + 850, func() -> void:
						_overlay.set(&"_target_id", "")
						_overlay.call(&"_refresh_back")
						var b := _overlay.get_node(^"Card/Margin/Column/Footer/Back") as Button
						print("tutorial_capture: basin button ", b.visible, " ", b.text))
					_at(f + 900, func() -> void: (_overlay.get_node(^"Card/Margin/Column/Footer/Back") as Button).pressed.emit())
					_at(f + 1400, func() -> void: print("tutorial_capture: after splash again in splash ", Editor.in_splash(), " session ", Session.state()))
					_shot_at = f + 880
			if _base == "splash_long":
				for k: int in range(1, 13):
					_at(f + 90 + k * 300, func() -> void: print("tutorial_capture: t+", k * 5, " s step ", _step_id(), " state ", Tutorial.state(), " session ", Session.state()))
		"finish":
			_at(f, func() -> void: _finish())
			_shot_at = f + 30
		"finish_again":
			_at(f, func() -> void: _finish())
			_at(f + 10, func() -> void: _finish())
			_at(f + 12, func() -> void: (_overlay.get_node(^"Finish/Scroll/Center/Panel/Margin/Column/LogRow/Copy") as Button).pressed.emit())
			_shot_at = f + 40
		"skip":
			_at(f, func() -> void: _overlay.call(&"_on_skip"))
			_shot_at = f + 30
		"skip_go":
			_at(f, func() -> void: _overlay.call(&"_on_skip_chosen", &"skip"))
			_at(f + 20, func() -> void: print("tutorial_capture: after skip screen ", Screens.base_name(), " tutorial ", Tutorial.state(), " dock open ", Editor.is_open(), " session ", Session.state()))
			_shot_at = f + 40
		"finish_open":
			_at(f, func() -> void: _finish())
			_at(f + 10, func() -> void: _overlay.call(&"_on_open"))
			_at(f + 60, func() -> void: print("tutorial_capture: after open screen ", Screens.base_name(), " mode ", Editor.dock_mode(), " path ", Editor.design_path(), " overlay finish ", (Screens.top().get_node(^"TutorialOverlay/Finish") as Control).visible))
			_shot_at = f + 120
		_:
			push_error("tutorial_capture: unknown state " + _state)
			return false
	_plan.sort_custom(func(a: Dictionary, b: Dictionary) -> bool: return int(a["at"]) < int(b["at"]))
	return true


## The facade has no way to finish a run on demand: stop it (so the poll sees no running tutorial, as after
## a real finish) and hand the overlay the signal the run would emit.
func _finish() -> void:
	Tutorial.stop()
	_overlay.call(&"_on_finished", "starters/fac22.bp.ron")
	print("tutorial_capture: blueprints ", Editor.browse("").map(func(e: Dictionary) -> String: return String(e.get("path", ""))))


func _widen_once() -> void:
	if _step_id() != "beam":
		return
	var st := int(_overlay.get(&"_point_station")) if _base == "beam_widen" else 0
	for h: Dictionary in Editor.handles():
		if String(h.get("id", "")) == "beam" and int(h.get("station", -1)) == st:
			var v := float(h.get("value", 0.0)) + maxf(float(h.get("step", 0.1)), 0.1)
			var applied: float = Editor.set_handle("beam", st, v, false)
			var fl: Dictionary = (Editor.gauges() as Dictionary).get("float", {})
			print("tutorial_capture: station ", st, " beam ", snappedf(applied, 0.01), " gm ", snappedf(float(fl.get("gm_m", 0.0)), 0.01), " float check ", fl.get("check"), " step ", _step_id())
			return


func _step_id() -> String:
	var s: Variant = Tutorial.step()
	return String((s as Dictionary).get("id", "")) if s is Dictionary else "<none>"


func _beam_state() -> String:
	var out := []
	for h: Dictionary in Editor.handles():
		if String(h.get("id", "")) == "beam":
			out.append("%d:%.3f" % [int(h.get("station", -1)), float(h.get("value", 0.0))])
	var u: Dictionary = Editor.undo_state()
	return "beam %s undo %s redo %s" % [str(out), str(u.get("can_undo", false)), str(u.get("can_redo", false))]


func _report() -> void:
	if not is_instance_valid(_overlay):
		print("tutorial_capture: state ", _state, " tutorial ", Tutorial.state(), "; the tutorial dock is gone, screen ", Screens.base_name())
		return
	var card := _overlay.get_node(^"Card") as Control
	print("tutorial_capture: state ", _state, " tutorial ", Tutorial.state(), " step ", _step_id())
	print("tutorial_capture: card visible ", card.visible, " rect ", Rect2(card.position, card.size))
	print("tutorial_capture: pointer ", _overlay.get(&"_target_rect"), " target ", _overlay.get(&"_target_id"))
	print("tutorial_capture: screen ", get_viewport().get_visible_rect())


func _click_first_tray_row() -> void:
	var tray := _dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray")
	var row: Control = _first_with_meta(tray, "editor.tray.")
	if row == null:
		print("tutorial_capture: no tray row")
		return
	var at := row.get_global_rect().get_center()
	var motion := InputEventMouseMotion.new()
	motion.position = at
	Input.parse_input_event(motion)
	var press := InputEventMouseButton.new()
	press.button_index = MOUSE_BUTTON_LEFT
	press.pressed = true
	press.position = at
	Input.parse_input_event.call_deferred(press)
	print("tutorial_capture: clicked ", row.get_meta(&"control_id"), " at ", at, " disabled ", row.get(&"disabled"))


func _first_with_meta(node: Node, prefix: String) -> Control:
	if node is Control and node.has_meta(&"control_id") and String(node.get_meta(&"control_id")).begins_with(prefix) and (node as Control).is_visible_in_tree():
		return node as Control
	for c: Node in node.get_children():
		var f := _first_with_meta(c, prefix)
		if f != null:
			return f
	return null


## Place a kit at the first valid cell, scanning from the deck down along the centreline.
func _place_kit(kit: StringName) -> bool:
	if not Editor.pick_kit(kit):
		return false
	var hull: Dictionary = Editor.hull()
	var size: Vector3i = hull.get("size_cells", Vector3i(44, 12, 12))
	var mid := size.z / 2
	for y: int in range(size.y + 2, -1, -1):
		for dx: int in range(0, size.x, 2):
			for x: int in [size.x / 2 + dx / 2, size.x / 2 - dx / 2 - 1]:
				for z: int in [mid, mid - 1]:
					var g: Dictionary = Editor.ghost_move(Vector3i(x, y, z))
					if bool(g.get("valid", false)):
						var placed: PackedInt32Array = Editor.ghost_place()
						Editor.ghost_cancel()
						return not placed.is_empty()
	Editor.ghost_cancel()
	return false


func _light(vp: SubViewport) -> void:
	var tokens: Dictionary = JSON.parse_string(FileAccess.get_file_as_string("res://art/tokens/world_tokens.json"))
	var dusk := {}
	for e: Dictionary in tokens["sky"]:
		if e["time"] == "dusk":
			dusk = e
	var lin := func(entry: Dictionary) -> Color:
		var v: Array = entry["linear_rgb"]
		return Color(float(v[0]), float(v[1]), float(v[2]))
	var sun := DirectionalLight3D.new()
	sun.light_color = (lin.call(dusk["key_light"]) as Color).linear_to_srgb()
	sun.light_energy = 2.0
	sun.shadow_enabled = true
	sun.directional_shadow_max_distance = 300.0
	sun.rotation_degrees = Vector3(-14.0, 125.0, 0.0)
	vp.add_child(sun)
	var shader := Shader.new()
	shader.code = SKY
	var mat := ShaderMaterial.new()
	mat.shader = shader
	for k: String in ["zenith", "horizon"]:
		var c: Color = lin.call(dusk[k])
		mat.set_shader_parameter(k, Vector3(c.r, c.g, c.b))
	var kc: Color = lin.call(dusk["key_light"])
	mat.set_shader_parameter("sun_col", Vector3(kc.r, kc.g, kc.b))
	var sky := Sky.new()
	sky.sky_material = mat
	var env := Environment.new()
	env.background_mode = Environment.BG_SKY
	env.sky = sky
	env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	env.tonemap_mode = Environment.TONE_MAPPER_AGX
	var we := WorldEnvironment.new()
	we.environment = env
	vp.add_child(we)
