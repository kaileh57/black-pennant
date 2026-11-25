extends Node
## Windowed capture of the dry dock screen in one state, for review against DESIGN.md "Interface and visual
## design". Units ui-editor-shell, ui-editor-tray, ui-editor-hull. Run from the repository root with a
## throwaway HOME (saves write user:// files):
##   HOME=/tmp/bp-dock godot --path game --resolution 1600x900 res://test/ui/dock_ui_capture.tscn -- \
##       --state=<state> --out=<folder> [--bp=starters/fac22.bp.ron]
## States: parts, selected, placing, context, why, hull, planes, sculpt, bake, armour, colour, picker,
## markings, overlays, search_empty, closed, splash; any state with _hc (high contrast) or _s200 (UI scale
## 200%) appended. The stage gets the dock capture's dusk light here, as res://world/dock_stage.tscn has none
## of its own yet. Waits for the design to show, writes <folder>/dock_<state>.png and quits; exits 1 on a
## failed setup. Prints what it set up and the frame time of the screen's main-thread work.

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

var _state := "parts"
var _base := "parts"
var _out := "user://captures"
var _bp := "starters/fac22.bp.ron"
var _frame := 0
var _dock: Control
var _shown := false
var _shot_at := -1
var _step_at := -1
var _step2_at := -1


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--state="):
			_state = arg.trim_prefix("--state=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--bp="):
			_bp = arg.trim_prefix("--bp=")
	_base = _state.trim_suffix("_hc").trim_suffix("_s200").trim_suffix("_hc")
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
	if _frame > 2400:
		push_error("dock_ui_capture: timed out")
		get_tree().quit(1)
		return
	if _shot_at < 0 and _frame > 4 and (_shown or _frame > 600 or _base == "closed"):
		_step_at = _frame + 30
		_shot_at = _frame + 90
	if _frame == _step_at:
		_apply_state()
		if _base == "interact":
			_shot_at = _frame + 900
		if _base == "splash_tune" or _base == "splash_end" or _base == "after_splash":
			_step2_at = _frame + 240
			_shot_at = _frame + (1300 if _base == "after_splash" else 420)
	if _frame == _step2_at:
		if _base == "splash_tune":
			(_node("SplashUi/Tune") as Button).button_pressed = true
		else:
			Editor.splash_end()
		print("dock_ui_capture: second step at frame ", _frame, " session ", Session.state())
	if _frame == _step2_at + 240 and _base == "after_splash":
		print("dock_ui_capture: back in the dock, session ", Session.state(), " in splash ", Editor.in_splash())
		_interact()
	if _frame == _shot_at:
		var path := _out.path_join("dock_%s.png" % _state)
		get_viewport().get_texture().get_image().save_png(path)
		print("dock_ui_capture: saved ", path)
		Editor.close()
		get_tree().quit(0)


func _setup() -> bool:
	if _base != "closed":
		var issues: Array = Screens.open_dry_dock(Bp.DOCK_SANDBOX, {"blueprint": _bp})
		for i: Dictionary in issues:
			if bool(i.get("fatal", false)):
				push_error("dock_ui_capture: open refused " + str(issues))
				return false
	else:
		Editor.close()
		Screens.change(&"dry_dock")
	_dock = Screens.top() as Control
	if _dock == null:
		push_error("dock_ui_capture: no dock screen")
		return false
	var view: Node = _dock.get_node_or_null(^"Scroll/Frame/Body/Centre/Stage/Viewport/DockStage/View")
	if view != null:
		view.connect(&"design_shown", func(_r: int) -> void: _shown = true)
	_light(_dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport") as SubViewport)
	return true


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
	env.ssao_enabled = true
	var we := WorldEnvironment.new()
	we.environment = env
	vp.add_child(we)


func _node(path: String) -> Node:
	return _dock.get_node_or_null(NodePath(path))


func _view_centre() -> Vector2:
	var display := _node("Scroll/Frame/Body/Centre/Stage/Display") as Control
	return display.size * 0.5


func _apply_state() -> void:
	var t0 := Time.get_ticks_usec()
	var tabs := _node("Scroll/Frame/Body/LeftDock/Tabs") as TabContainer
	match _base:
		"selected", "context", "why":
			Editor.select_parts(PackedInt32Array([_first_part()]), false)
			if _base == "context":
				var display := _node("Scroll/Frame/Body/Centre/Stage/Display") as Control
				_node("Context").call(&"open_dock", display.get_global_transform_with_canvas() * _view_centre(), {"kind": &"part", "slot": _first_part()}, false)
			elif _base == "why":
				_node("Context").call(&"show_why_dock", Vector2(700, 300), _first_part())
		"placing":
			_node("Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray").call(&"pick", &"kit.gun", true)
			_dock.call(&"_move_ghost", _view_centre() + Vector2(120, -40))
		"hull":
			tabs.current_tab = 1
		"planes":
			tabs.current_tab = 1
			_node("Scroll/Frame/Body/LeftDock/Tabs/Hull").call(&"_on_mode", 1)
		"sculpt", "bake":
			tabs.current_tab = 1
			_node("Scroll/Frame/Body/LeftDock/Tabs/Hull").call(&"_on_mode", 2)
			if _base == "bake":
				_node("Scroll/Frame/Body/LeftDock/Tabs/Hull").call(&"_confirm_bake")
		"armour":
			tabs.current_tab = 2
		"colour", "picker":
			tabs.current_tab = 2
			_node("Scroll/Frame/Body/LeftDock/Tabs/Paint").call(&"_on_mode", 1)
			if _base == "picker":
				_node("Scroll/Frame/Body/LeftDock/Tabs/Paint").call(&"_on_swatch", 9)
		"markings":
			tabs.current_tab = 2
			_node("Scroll/Frame/Body/LeftDock/Tabs/Paint").call(&"_on_mode", 2)
		"overlays":
			_node("Scroll/Frame/Body/Centre/Overlays").call(&"ensure", Bp.OVERLAY_ARCS | Bp.OVERLAY_CG_CB | Bp.OVERLAY_WATERLINE)
		"search_empty":
			var search := _node("Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray/Search") as LineEdit
			search.text = "zzzz"
			search.text_changed.emit("zzzz")
		"interact":
			_interact()
		"splash", "splash_tune", "splash_end", "after_splash":
			_dock.call(&"_do_splash")
	print("dock_ui_capture: state %s applied in %.2f ms" % [_state, (Time.get_ticks_usec() - t0) / 1000.0])


func _mouse(display: Control, at: Vector2, button: MouseButton, pressed: bool) -> void:
	var e := InputEventMouseButton.new()
	e.button_index = button
	e.pressed = pressed
	e.position = at
	e.global_position = display.get_global_transform_with_canvas() * at
	display.gui_input.emit(e)


func _click(display: Control, at: Vector2, secondary: bool) -> void:
	var b := MOUSE_BUTTON_RIGHT if secondary else MOUSE_BUTTON_LEFT
	_mouse(display, at, b, true)
	_mouse(display, at, b, false)


func _drag(display: Control, from: Vector2, to: Vector2) -> void:
	_mouse(display, from, MOUSE_BUTTON_LEFT, true)
	for i in range(1, 11):
		var m := InputEventMouseMotion.new()
		m.position = from.lerp(to, i / 10.0)
		m.relative = (to - from) / 10.0
		m.button_mask = MOUSE_BUTTON_MASK_LEFT
		display.gui_input.emit(m)
	_mouse(display, to, MOUSE_BUTTON_LEFT, false)


func _part(slot: int) -> Dictionary:
	for p: Dictionary in Editor.parts():
		if int(p["slot"]) == slot:
			return p
	return {}


func _first_part() -> int:
	var parts: Array = Editor.parts()
	for p: Dictionary in parts:
		if String(p.get("part", "")).begins_with("part.gun") or String(p.get("part", "")).contains("gun"):
			return int(p.get("slot", 0))
	return int(parts[0].get("slot", 0)) if not parts.is_empty() else 0


## Drive the view the way a player does, through the Display's input: click a part, box select, drag a hull
## handle, zoom and orbit, open the context menu. Prints what each step changed.
func _interact() -> void:
	var display := _node("Scroll/Frame/Body/Centre/Stage/Display") as Control
	var view: Node = _node("Scroll/Frame/Body/Centre/Stage/Viewport/DockStage/View")
	var part_at := Vector2(-1, -1)
	var empty_at := Vector2(8, 8)
	for y in range(40, int(display.size.y) - 40, 12):
		for x in range(40, int(display.size.x) - 40, 12):
			var hit: Dictionary = view.call(&"pick", Vector2(x, y))
			if hit["kind"] == &"part" and part_at.x < 0:
				part_at = Vector2(x, y)
	print("interact: part at ", part_at)
	_click(display, part_at, false)
	print("interact: selected after click ", Editor.selection()["parts"])
	_drag(display, empty_at, display.size * 0.8)
	print("interact: selected after box ", (Editor.selection()["parts"] as PackedInt32Array).size())
	var depth0: int = Editor.undo_state()["depth"]
	(_node("Scroll/Frame/Body/LeftDock/Tabs") as TabContainer).current_tab = 1
	await get_tree().process_frame
	await get_tree().process_frame
	var handles: Array = view.call(&"handle_points")
	var beam_at := Vector2(-1, -1)
	for h: Dictionary in handles:
		if h["id"] == "beam" and bool(h["visible"]):
			beam_at = h["screen"]
			break
	var before: float = Editor.hull()["stations"][0]["beam_m"]
	print("interact: beam handle at ", beam_at, " pick ", view.call(&"pick", beam_at)["kind"])
	_drag(display, beam_at, beam_at + Vector2(60, -20))
	await get_tree().process_frame
	print("interact: undo depth ", depth0, " -> ", Editor.undo_state()["depth"], " station 0 beam ", before, " -> ", Editor.hull()["stations"][0]["beam_m"])
	var wheel := InputEventMouseButton.new()
	wheel.button_index = MOUSE_BUTTON_WHEEL_UP
	wheel.pressed = true
	wheel.position = display.size * 0.5
	display.gui_input.emit(wheel)
	var pan := InputEventPanGesture.new()
	pan.delta = Vector2(6, 0)
	pan.position = display.size * 0.5
	display.gui_input.emit(pan)
	print("interact: camera log ", view.call(&"camera_log"))
	_click(display, part_at, true)
	print("interact: context menu shown ", (_node("Context/Menu") as PopupMenu).visible)
	(_node("Context") as Node).call(&"close")
	await _more(display, view, part_at)


func _key(code: Key, shift := false, command := false) -> void:
	for pressed: bool in [true, false]:
		var e := InputEventKey.new()
		e.keycode = code
		e.physical_keycode = code
		e.shift_pressed = shift
		if command:
			e.meta_pressed = OS.get_name() == "macOS"
			e.ctrl_pressed = OS.get_name() != "macOS"
		e.pressed = pressed
		Input.parse_input_event(e)
	await get_tree().process_frame
	await get_tree().process_frame


func _more(display: Control, view: Node, part_at: Vector2) -> void:
	var tabs := _node("Scroll/Frame/Body/LeftDock/Tabs") as TabContainer
	tabs.current_tab = 0
	await get_tree().process_frame
	# Drag a part: the selection moves by whole cells on the plane under it.
	Editor.clear_selection()
	# Main-thread cost of one edit on this design: the frame after a committed handle change, against idle.
	var idle := 0.0
	for i in 10:
		var a := Time.get_ticks_usec()
		await get_tree().process_frame
		idle += (Time.get_ticks_usec() - a) / 10000.0
	var cost := 0.0
	for i in 5:
		var v: float = Editor.handles()[0]["value"]
		var a := Time.get_ticks_usec()
		Editor.set_handle(Editor.handles()[0]["id"], int(Editor.handles()[0]["station"]), v + (0.1 if i % 2 == 0 else -0.1), false)
		await get_tree().process_frame
		cost += (Time.get_ticks_usec() - a) / 5000.0
	print("interact: frame after an edit %.1f ms, idle frame %.1f ms" % [cost, idle])
	for tab in 3:
		tabs.current_tab = tab
		await get_tree().process_frame
		var a := Time.get_ticks_usec()
		Editor.set_design_name("Cost check %d" % tab)
		await get_tree().process_frame
		print("interact: tab %d frame after a rename %.1f ms" % [tab, (Time.get_ticks_usec() - a) / 1000.0])
	var calls := [[_dock, &"_refresh_chrome"], [_dock, &"_refresh_launch"], [_dock, &"_refresh_slices"], [_dock, &"_refresh_handles"],
		[_node("Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray"), &"_reload"], [_node("Scroll/Frame/Body/LeftDock/Tabs/Hull"), &"_refresh"],
		[_node("Scroll/Frame/Body/LeftDock/Tabs/Parts/Inspector"), &"_refresh"], [_node("Scroll/Frame/Body/LeftDock/Tabs/Paint"), &"_refresh"]]
	Editor.select_parts(PackedInt32Array([0]), false)
	for c: Array in calls:
		var a := Time.get_ticks_usec()
		if c[1] == &"_reload":
			(c[0] as Node).call(c[1], false)
		else:
			(c[0] as Node).call(c[1])
		print("interact: %s.%s %.2f ms" % [(c[0] as Node).name, c[1], (Time.get_ticks_usec() - a) / 1000.0])
	var again := Time.get_ticks_usec()
	_node("Scroll/Frame/Body/LeftDock/Tabs/Parts/Inspector").call(&"_refresh")
	print("interact: Inspector._refresh again %.2f ms" % ((Time.get_ticks_usec() - again) / 1000.0))
	tabs.current_tab = 0
	var hit: Dictionary = view.call(&"pick", part_at)
	if hit["kind"] != &"part":
		print("interact: no part under the cursor after the camera moved; skipping the move checks")
		return
	var slot := int(hit["slot"])
	var before: Vector3i = _part(slot)["cell"]
	var t0 := Time.get_ticks_usec()
	_drag(display, part_at, part_at + Vector2(-40, 0))
	print("interact: move drag %.1f ms; cell %s -> %s; status '%s'; hit %s" % [(Time.get_ticks_usec() - t0) / 1000.0, before, _part(slot)["cell"], (_node("Scroll/Frame/TopBar/Margin/Row/Status") as Label).text, hit])
	# A part on the open deck moves freely: try the move on the gun found by the first part search.
	var gun_at := part_at
	for y in range(40, int(display.size.y) - 40, 10):
		for x in range(int(display.size.x * 0.55), int(display.size.x) - 40, 10):
			var h: Dictionary = view.call(&"pick", Vector2(x, y))
			if h["kind"] == &"part":
				gun_at = Vector2(x, y)
				break
		if gun_at != part_at:
			break
	var gslot := int(view.call(&"pick", gun_at)["slot"])
	var g0: Vector3i = _part(gslot)["cell"]
	_drag(display, gun_at, gun_at + Vector2(0, 30))
	print("interact: second move cell %s -> %s; status '%s'" % [g0, _part(gslot)["cell"], (_node("Scroll/Frame/TopBar/Margin/Row/Status") as Label).text])
	# Keys through the input pipeline: R turns the selected part, T tilts, Cmd/Ctrl+Z undoes, Delete removes.
	Editor.select_parts(PackedInt32Array([slot]), false)
	var yaw0: int = _part(slot)["yaw"]
	await _key(KEY_R)
	print("interact: R yaw ", yaw0, " -> ", _part(slot).get("yaw"))
	var depth: int = Editor.undo_state()["depth"]
	await _key(KEY_Z, false, true)
	print("interact: undo depth ", depth, " -> ", Editor.undo_state()["depth"])
	await _key(KEY_Z, true, true)
	print("interact: redo depth -> ", Editor.undo_state()["depth"])
	await _key(KEY_C, false, true)
	await _key(KEY_V, false, true)
	print("interact: paste ghost ", Editor.ghost() != null)
	await _key(KEY_ESCAPE)
	print("interact: Esc drops ghost ", Editor.ghost() == null)
	var n0 := (Editor.parts() as Array).size()
	await _key(KEY_DELETE)
	print("interact: Delete parts ", n0, " -> ", (Editor.parts() as Array).size())
	await _key(KEY_ESCAPE)
	# Tray drop: drag data from a kit row onto the view places it.
	var data := {"tray_id": &"kit.gun", "is_kit": true}
	var at := display.size * Vector2(0.62, 0.45)
	var can := bool(_dock.call(&"_can_drop_on_view", at, data))
	var n1 := (Editor.parts() as Array).size()
	_dock.call(&"_drop_on_view", at, data)
	print("interact: drop accepted ", can, " parts ", n1, " -> ", (Editor.parts() as Array).size(), " ghost reason ", Editor.ghost())
	await _key(KEY_ESCAPE)
	# Paint, armour and sculpt strokes, a marking click.
	tabs.current_tab = 2
	await get_tree().process_frame
	var paint := _node("Scroll/Frame/Body/LeftDock/Tabs/Paint")
	var hull_at := Vector2(-1, -1)
	for y in range(60, int(display.size.y) - 60, 10):
		for x in range(60, int(display.size.x) - 60, 10):
			if view.call(&"pick", Vector2(x, y))["kind"] == &"cell":
				hull_at = Vector2(x, y)
				break
		if hull_at.x >= 0:
			break
	for mode in 2:
		paint.call(&"_on_mode", mode)
		await get_tree().process_frame
		var d0: int = Editor.undo_state()["depth"]
		t0 = Time.get_ticks_usec()
		_drag(display, hull_at, hull_at + Vector2(30, 6))
		await get_tree().process_frame
		print("interact: paint mode %d stroke %.1f ms, undo %d -> %d, %s" % [mode, (Time.get_ticks_usec() - t0) / 1000.0, d0, Editor.undo_state()["depth"], paint.get(&"_last_stroke")])
	paint.call(&"_on_mode", 2)
	await get_tree().process_frame
	var m0 := (Editor.markings() as Array).size()
	_click(display, hull_at, false)
	print("interact: marking ", m0, " -> ", (Editor.markings() as Array).size())
	tabs.current_tab = 1
	var hull := _node("Scroll/Frame/Body/LeftDock/Tabs/Hull")
	hull.call(&"_on_mode", 2)
	await get_tree().process_frame
	var d1: int = Editor.undo_state()["depth"]
	_click(display, hull_at, false)
	await get_tree().process_frame
	print("interact: sculpt click undo ", d1, " -> ", Editor.undo_state()["depth"])
	# Plane drag: add a deck, then drag it up.
	hull.call(&"_on_mode", 1)
	await get_tree().process_frame
	var deck := int(Editor.add_plane(&"deck", 1.5))
	await get_tree().process_frame
	Editor.set_tool(Bp.TOOL_PLANES)
	view.call(&"set_overlays", Bp.OVERLAY_COMPARTMENTS)
	await get_tree().process_frame
	await get_tree().process_frame
	var plane_at := Vector2(-1, -1)
	for y in range(40, int(display.size.y) - 40, 8):
		for x in range(40, int(display.size.x) - 40, 8):
			var h: Dictionary = view.call(&"pick", Vector2(x, y))
			if h["kind"] == &"deck":
				plane_at = Vector2(x, y)
				break
		if plane_at.x >= 0:
			break
	var decks0: PackedFloat32Array = Editor.hull()["decks"]
	if plane_at.x >= 0:
		_drag(display, plane_at, plane_at + Vector2(0, -40))
	print("interact: deck ", deck, " at ", plane_at, " decks ", decks0, " -> ", Editor.hull()["decks"])
	# Save a stock design: it becomes a user file.
	_dock.call(&"_on_save")
	print("interact: saved path ", Editor.design_path(), " dirty ", Editor.is_dirty())
