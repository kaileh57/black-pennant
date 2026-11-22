extends Node
## Windowed capture of the dry-dock 3D view (unit gd-editor-view): opens a sandbox dock on a starter, shows
## res://world/dock_stage.tscn under a dusk sky lit from the world tokens, and saves PNGs of the plain view,
## each overlay group, a deck slice, a ghost, the framing views and a thumbnail:
##   godot --path game --resolution 1600x900 res://test/capture/dock_capture.tscn -- --out=/abs/dir [--bp=starters/destroyer90.bp.ron]
## Prints what picking, cell_at, handle_points and legend return, and the view's frame time. Headless, and
## when instanced by check_project, it does nothing.

var _out := "user://captures"
var _bp := "starters/destroyer90.bp.ron"
var _stage: Node3D
var _view: BpEditorView
var _frame := 0
var _step := 0
var _wait := 0
var _active := false
var _tokens: Dictionary = {}
var _shown := false
var _hull_at := Vector2(800, 450)
var _part_slot := -1


## A viewport position on the design near the centre, and the slot of a part found on the way.
func _find_hull() -> void:
	for r in range(0, 400, 8):
		for k in 16:
			var p := Vector2(800, 450) + Vector2(cos(k * TAU / 16.0), sin(k * TAU / 16.0)) * r
			var hit: Dictionary = _view.pick(p)
			if hit["kind"] == &"part" and _part_slot < 0:
				_part_slot = int(hit["slot"])
			if hit["kind"] == &"cell" and _hull_at == Vector2(800, 450):
				_hull_at = p
			if _part_slot >= 0 and _hull_at != Vector2(800, 450):
				return


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--bp="):
			_bp = arg.trim_prefix("--bp=")
	DirAccess.make_dir_recursive_absolute(_out)
	_tokens = JSON.parse_string(FileAccess.get_file_as_string("res://art/tokens/world_tokens.json"))
	_build_environment()
	_stage = (load("res://world/dock_stage.tscn") as PackedScene).instantiate() as Node3D
	add_child(_stage)
	_view = _stage.get_node("View") as BpEditorView
	_view.design_shown.connect(func(_rev: int) -> void: _shown = true)
	var issues: Array = Editor.open(Bp.DOCK_SANDBOX, {"blueprint": _bp})
	for i: Dictionary in issues:
		if bool(i["fatal"]):
			push_error("dock_capture: open refused: " + str(issues))
			get_tree().quit(1)
			return
	_active = true


func _lin(entry: Dictionary) -> Color:
	var v: Array = entry["linear_rgb"]
	return Color(float(v[0]), float(v[1]), float(v[2]))


func _sky(time: String) -> Dictionary:
	for e: Dictionary in _tokens["sky"]:
		if e["time"] == time:
			return e
	return {}


func _build_environment() -> void:
	var dusk := _sky("dusk")
	var sun := DirectionalLight3D.new()
	sun.light_color = _lin(dusk["key_light"]).linear_to_srgb()
	sun.light_energy = 2.0
	sun.shadow_enabled = true
	sun.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_4_SPLITS
	sun.directional_shadow_max_distance = 300.0
	sun.shadow_blur = 1.0
	sun.rotation_degrees = Vector3(-14.0, 125.0, 0.0)
	add_child(sun)
	var sky_shader := Shader.new()
	sky_shader.code = """
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
	var sky_mat := ShaderMaterial.new()
	sky_mat.shader = sky_shader
	for k: String in ["zenith", "horizon"]:
		var c := _lin(dusk[k])
		sky_mat.set_shader_parameter(k, Vector3(c.r, c.g, c.b))
	var kc := _lin(dusk["key_light"])
	sky_mat.set_shader_parameter("sun_col", Vector3(kc.r, kc.g, kc.b))
	var sky := Sky.new()
	sky.sky_material = sky_mat
	var env := Environment.new()
	env.background_mode = Environment.BG_SKY
	env.sky = sky
	env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	env.reflected_light_source = Environment.REFLECTION_SOURCE_SKY
	env.tonemap_mode = Environment.TONE_MAPPER_AGX
	env.ssao_enabled = true
	var we := WorldEnvironment.new()
	we.environment = env
	add_child(we)
	get_viewport().msaa_3d = Viewport.MSAA_2X


func _shot(name: String) -> void:
	var image := get_viewport().get_texture().get_image()
	var path := _out.path_join("dock_%s.png" % name)
	image.save_png(path)
	print("dock_capture: saved ", path)


## Steps: each sets something up, waits some frames, then shoots.
func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame > 3000:
		push_error("dock_capture: timed out at step %d" % _step)
		_end(1)
		return
	if _wait > 0:
		_wait -= 1
		return
	var c := Vector2(800, 450)
	match _step:
		0:
			if not _shown and _frame < 1200:
				return
			print("dock_capture: design shown at frame ", _frame, " ", _shown)
			_wait = 30
		1:
			_shot("plain")
			var t0 := Time.get_ticks_usec()
			_find_hull()
			print("dock_capture: hull search ", (Time.get_ticks_usec() - t0) / 1000.0, " ms; part slot ", _part_slot)
			print("dock_capture: pick ", _view.pick(_hull_at))
			print("dock_capture: cell_at ", _view.cell_at(_hull_at))
			print("dock_capture: brush ", _view.brush_cells(_hull_at, 3).size())
			print("dock_capture: handles ", _view.handle_points().size())
			print("dock_capture: parts in rect ", _view.parts_in_rect(Rect2(0, 0, 1600, 900)).size())
			_view.set_overlays(Bp.OVERLAY_WATERLINE | Bp.OVERLAY_CG_CB | Bp.OVERLAY_ARCS)
			_wait = 240
		2:
			_shot("arcs_waterline_cg")
			print("dock_capture: legend ", _view.legend())
			_view.set_overlay_focus(PackedInt32Array([_part_slot]))
			_view.set_overlays(Bp.OVERLAY_ARCS | Bp.OVERLAY_BLIND_SECTORS)
			_wait = 30
		3:
			_shot("arcs_focus")
			_view.set_overlay_focus(PackedInt32Array())
			_view.set_overlays(Bp.OVERLAY_XRAY)
			_wait = 30
		4:
			_shot("xray")
			_view.set_overlays(Bp.OVERLAY_HEATMAP)
			_wait = 30
		5:
			_shot("heatmap")
			_view.set_overlays(Bp.OVERLAY_COMPARTMENTS | Bp.OVERLAY_BLIND_SECTORS)
			_wait = 30
		6:
			_shot("compartments_blind")
			_view.set_overlays(Bp.OVERLAY_SIGNATURE)
			_view.frame(&"top")
			_wait = 30
		7:
			_shot("signature_top")
			_view.set_overlays(0)
			_view.frame(&"all")
			_view.set_deck_slice(0)
			_wait = 60
		8:
			_shot("deck_slice")
			_view.set_deck_slice(-1)
			Editor.set_symmetry(Bp.SYMMETRY_MIRROR, 2)
			Editor.pick_kit("kit.gun")
			var size: Vector3i = Editor.hull()["size_cells"]
			var found := false
			for x in range(size.x * 2 / 3, size.x - 4):
				for y in range(0, size.y + 4):
					var g: Dictionary = Editor.ghost_move(Vector3i(x, y, size.z / 2))
					if bool(g["valid"]):
						found = true
						break
				if found:
					break
			print("dock_capture: ghost valid ", found, " ", Editor.ghost())
			_wait = 30
		9:
			_shot("ghost")
			var size: Vector3i = Editor.hull()["size_cells"]
			Editor.ghost_move(Vector3i(size.x / 2, 1, size.z / 2))
			_wait = 10
		10:
			_shot("ghost_invalid")
			Editor.ghost_cancel()
			_view.frame(&"below")
			_view.set_overlays(Bp.OVERLAY_ARCS | Bp.OVERLAY_WATERLINE)
			_wait = 30
		11:
			_shot("below")
			_view.set_overlays(0)
			_view.frame(&"stern")
			_wait = 20
		12:
			_shot("stern")
			_view.frame(&"all")
			for i in 10:
				_view.pan_gesture(Vector2(-12.0, 0.0), false, false, c)
			_wait = 20
		13:
			_shot("orbited")
			var t0 := Time.get_ticks_usec()
			var img: Image = _view.thumbnail(Vector2i(512, 288))
			print("dock_capture: thumbnail ", img != null, " ", (Time.get_ticks_usec() - t0) / 1000.0, " ms")
			if img != null:
				img.save_png(_out.path_join("dock_thumbnail.png"))
				print("dock_capture: thumbnail pixels ", img.get_size(), " ", img.get_pixel(img.get_width() / 2, img.get_height() / 2), " ", img.get_pixel(2, 2))
				var again: Image = _view.thumbnail(Vector2i(512, 288))
				print("dock_capture: thumbnail again ", again.get_pixel(again.get_width() / 2, again.get_height() / 2))
			print("dock_capture: camera log ", _view.camera_log())
			_wait = 10
		14:
			_end(0)
			return
	_step += 1


func _end(code: int) -> void:
	_active = false
	Editor.close()
	get_tree().quit(code)
