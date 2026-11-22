extends SceneTree
## In-engine check of the sortie world view and the sea (Rust `BpWorldView`, driven by `BpWorldCheck`):
##   godot --headless --path . -s res://test/tools/world_check.gd
## Plays a scripted sortie for 480 frames through a `BpWorldView` in the tree, checks the camera, picking,
## markers, the aim readout, signals (their handlers call straight back into the view) and the scene's
## lifetime, and exits 1 on any failure. For six frames near the end the tree is paused: the view stops, and
## the shader clock must hold the drawn sea still by keeping `bp_waves_t_ref` level with `TIME`.
## With a window instead of --headless, and `-- --capture=<prefix>` after the script, it also adds a sun,
## a sky and floats placed on the simulation's sea surface, and saves <prefix>_a.png (looking toward the
## low sun), <prefix>_b.png (the other hull and the island), <prefix>_d.png (low over the floats) and
## <prefix>_c.png (after the origin rebase). The floats are 3 m posts centred on the simulation's surface:
## each must stand 1.5 m out of the drawn water. That is the sea, the wave uniforms and the shader clock
## agreeing. Three plates above the floats measure the same on the GPU: each is white where the wave include
## puts the surface within 10 mm, 2 mm or 0.5 mm of the simulation's height, and the run fails outside 10 mm.

const FRAMES := 480
const TOKENS := "res://art/tokens/world_tokens.json"
## Plates hover this far above the simulation's surface and test the wave include on the GPU against it:
## white where the two agree within the plate's tolerance.
const PLATE_LIFT_M := 3.0
const PLATE_TOLERANCES_M: Array[float] = [0.01, 0.002, 0.0005]
const PLATE_SHADER := """
shader_type spatial;
render_mode unshaded, cull_disabled, fog_disabled, shadows_disabled;
#include "res://shaders/gen/waves.gdshaderinc"
uniform float lift = 3.0;
uniform float tolerance = 0.01;
void fragment() {
	vec3 centre = MODEL_MATRIX[3].xyz;
	float residual = (centre.y - lift) - wave_height(centre.xz, TIME);
	ALBEDO = vec3(abs(residual) < tolerance ? 1.0 : 0.0);
}
"""

var _check: BpWorldCheck
var _view: BpWorldView
var _camera: Camera3D
var _frame := 0
var _failures := 0
var _capture := ""
var _started := false
var _counts: Dictionary[StringName, int] = {&"environment": 0, &"rebased": 0, &"lightning": 0, &"warmed": 0, &"view": 0}
var _reentered := 0
var _floats: Array[MeshInstance3D] = []
var _plates: Array[MeshInstance3D] = []
var _env: Environment
var _sun: DirectionalLight3D
var _sky: ShaderMaterial
var _tokens: Dictionary = {}
var _held_seen := false
var _min_above_sea := 1000.0
var _adapter_before_pause := 0.0
var _paused_sea_checked := 0


func _initialize() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--capture="):
			_capture = arg.trim_prefix("--capture=")
	_view = BpWorldView.new()
	root.add_child(_view)
	_camera = Camera3D.new()
	_camera.far = 20000.0
	root.add_child(_camera)
	_camera.make_current()
	_view.bind_camera(_camera)
	_view.environment_changed.connect(_on_environment)
	_view.origin_rebased.connect(_on_rebased)
	_view.lightning.connect(_on_lightning)
	_view.warmed_up.connect(_on_warmed)
	_view.view_changed.connect(_on_view)
	_check = BpWorldCheck.new()
	_started = _check.start(_view)
	if not _started:
		_fail("no stock design with a gun could be built")
	if _capture != "":
		_add_stage()
	# Asked for before the first frame, as the loading screen does.
	_view.warm_up()
	if _counts[&"warmed"] != 0:
		_fail("warmed_up was emitted inside warm_up()")


# --- signal handlers: each calls back into the view, as scene scripts do ---

func _on_environment() -> void:
	_counts[&"environment"] += 1
	var env: Dictionary = _view.environment()
	if env.has("preset") and _view.to_render(Vector2.ZERO, 0.0) is Vector3:
		_reentered += 1
	_apply_environment(env)


func _on_rebased(shift: Vector3) -> void:
	_counts[&"rebased"] += 1
	if shift.length() > 500.0 and _view.render_origin() is Vector2:
		_reentered += 1
	for marker: MeshInstance3D in _floats + _plates:
		marker.position += shift


func _on_lightning(_intensity: float, _duration_s: float, _bearing_deg: float) -> void:
	_counts[&"lightning"] += 1


func _on_warmed() -> void:
	_counts[&"warmed"] += 1
	if _view.stats().has("adapter_ms_last"):
		_reentered += 1


func _on_view(_view_id: int) -> void:
	_counts[&"view"] += 1
	if _view.orbit_state().has("yaw_deg"):
		_reentered += 1


# --- the frames ---

func _process(delta: float) -> bool:
	_frame += 1
	if not _started:
		quit(1)
		return true
	if _frame <= FRAMES:
		_check.step(delta)
		_place_floats()
		_at_frame()
		return false
	if _frame == FRAMES + 1:
		_final_checks()
		_check.finish()
		return false
	if _frame < FRAMES + 4:
		return false
	# The session went idle three frames ago: the view has freed its scene.
	if not _view.pick(Vector2(100.0, 100.0)).get("kind", &"") == &"none":
		_fail("a view with no scene still picks something")
	if _view.lut() != null:
		_fail("a view with no scene still has a LUT")
	_view.queue_free()
	_check = null
	print("world check: ", "FAILED" if _failures > 0 else "ok", " (", _failures, " failures)")
	quit(1 if _failures > 0 else 0)
	return true


func _at_frame() -> void:
	var report: Dictionary = _check.report(_view)
	if _frame > 3:
		_min_above_sea = minf(_min_above_sea, float(report.get("camera_above_sea_m", 1000.0)))
	match _frame:
		20:
			# The opening distance fits the hull: its longest side across 45% of a 16:9 view's width at a
			# 60 degree field of view, which is 1.0825 times that side, kept within 30 m to 600 m.
			var size: Vector3 = report["own_size_m"]
			var framed: float = clampf(maxf(size.x, maxf(size.y, size.z)) * (9.0 / 16.0) / (2.0 * tan(deg_to_rad(30.0)) * 0.45), 30.0, 600.0)
			var opening: float = _view.orbit_state().get("distance_m", 0.0)
			if absf(opening - framed) > 0.1:
				_fail("the sortie opened at %f m, not framed on the %s m hull at %f m" % [opening, size, framed])
			_expect(report, "scene", true)
			_expect(report, "camera_valid", true)
			_expect(report, "wave_set_matches", true)
			_expect(report, "sea_state_shown", 4)
			_expect(_view.environment(), "preset", &"dusk_clear")
		90:
			# Looking toward the low sun from the default orbit: the hull under the middle of the view.
			_expect(_view.pick(report["own_screen"]), "kind", &"own")
			var canvas: Vector2 = report["canvas"]
			var sea: Dictionary = _view.pick(Vector2(canvas.x * 0.12, canvas.y * 0.9))
			_expect(sea, "kind", &"sea")
			var origin: Vector2 = report["origin"]
			if (sea["map"] as Vector2).distance_to(origin) > 6000.0:
				_fail("the sea pick is nowhere near the scene: %s" % [sea["map"]])
			if _view.sea_point(Vector2(canvas.x * 0.5, 2.0)) != null:
				_fail("sea_point above the horizon is not null")
			if _view.sea_point(Vector2(canvas.x * 0.5, canvas.y - 2.0)) == null:
				_fail("sea_point at the foot of the view is null")
			var back: Vector2 = _view.project(sea["map"], 0.0)
			if back.distance_to(Vector2(canvas.x * 0.12, canvas.y * 0.9)) > 40.0:
				_fail("project does not return near the picked sea point: %s" % [back])
			# The manual aim: the reticle on the sea, the forward gun held since frame 60.
			_view.set_reticle(Vector2(canvas.x * 0.5, canvas.y * 0.62))
		100:
			var aim: Dictionary = _view.aim_readout()
			_expect(aim, "valid", true)
			# The sortie opened framed on this hull, so the sea under the reticle is close: metres, not zero.
			if float(aim.get("range_m", 0.0)) < 5.0 or float(aim.get("time_of_flight_s", -1.0)) < 0.0:
				_fail("the aim readout has no range: %s" % [aim])
			if (report["aim_local"] as Vector3).length() < 0.99:
				_fail("no aim direction was left for Orders: %s" % [report["aim_local"]])
			_held_seen = true
			_shot("a")
			# Turn to look at the other hull, 900 m off the port bow.
			_view.orbit(40.0 - 200.0, 0.0)
			_view.zoom(-4.0)
		130:
			var markers: PackedFloat32Array = _view.track_markers()
			if markers.size() != 12:
				_fail("expected 2 track markers, got %d floats" % markers.size())
			else:
				# Ascending track number: 12 (the drawn hull, Hostile in range), 20 (designated).
				if int(markers[0]) != 12 or int(markers[6]) != 20:
					_fail("markers are not in track order: %s" % [markers])
				if int(markers[3]) != 1:
					_fail("the hull in view has an off-screen marker")
				if int(markers[5]) != 2 or int(markers[11]) != 0:
					_fail("marker ranks are %d and %d, expected 2 and 0" % [int(markers[5]), int(markers[11])])
				if absf(markers[4] - 900.0) > 150.0:
					_fail("range to the other hull is %f" % markers[4])
				var at_marker: Dictionary = _view.pick(Vector2(markers[1], markers[2]))
				_expect(at_marker, "kind", &"track")
				_expect(at_marker, "track", 12)
			_expect(_view.pick(report["enemy_screen"]), "kind", &"track")
			var stats: Dictionary = _view.stats()
			if int(stats.get("chunks", 0)) < 1 or int(stats.get("part_instances", 0)) < 1:
				_fail("nothing is drawn: %s" % [stats])
		200:
			_expect(report, "shore_window", true)
			if float(report.get("ground_at_island_m", 0.0)) < 20.0:
				_fail("the island is not in the sea's map")
			_shot("b")
			if _view.toggle_view() != Bp.VIEW_PLOT or not root.disable_3d:
				_fail("the plot view did not switch the 3D viewport off")
		210:
			# Under the plot table the units still run: the weather turn at 240 must not be missed.
			# A tier change rebuilds the clipmap and the effect pools in place (L1897-1901).
			if _check.retier(_view, Bp.QUALITY_LOW) != 8:
				_fail("the Low tier did not give an 8-level clipmap")
		225:
			if _check.retier(_view, Bp.QUALITY_HIGH) != 9:
				_fail("the High tier did not give a 9-level clipmap")
		235:
			# Back to Medium without telling the view: it must see the profile change on its next frame.
			_check.set_tier_quietly(Bp.QUALITY_MEDIUM)
			if _check.sea_levels(_view) != 9:
				_fail("the quiet tier change was applied before the view ran a frame")
		237:
			if _check.sea_levels(_view) != 10:
				_fail("the view did not follow the profile to the Medium tier's 10-level clipmap")
		260:
			if _view.toggle_view() != Bp.VIEW_BRIDGE or root.disable_3d:
				_fail("the bridge view did not switch the 3D viewport back on")
			_expect(report, "sea_state_shown", 5)
			_expect(report, "wave_set_matches", true)
			_expect(_view.environment(), "preset_to", &"dusk_overcast")
		300:
			_view.set_free_camera(true, report["origin"], 60.0, report["origin"] + Vector2(300.0, -200.0), 0.0)
		310:
			if float(report.get("camera_above_sea_m", 0.0)) < 30.0:
				_fail("the free camera is not where it was put")
			# Low over the floats, for the capture that shows them riding the drawn surface.
			var own: Vector2 = report["own_map"]
			_view.set_free_camera(true, own + Vector2(-95.0, 60.0), 9.0, own + Vector2(10.0, 78.0), 0.0)
		325:
			_shot("d")
			_read_plates("before the rebase")
			_view.set_free_camera(false, Vector2.ZERO, 0.0, Vector2.ZERO, 0.0)
			if _view.follow(2) != true or _view.follow(999) != false or _view.follow(Bp.NONE) != true:
				_fail("follow() accepts or refuses the wrong vessels")
		420:
			# Sixty frames after the 9 km move: a new origin, and everything still lines up.
			_expect(report, "wave_set_matches", true)
			_expect(_view.pick(report["own_screen"]), "kind", &"own")
			_shot("c")
			_read_plates("after the rebase")
		440:
			# A paused tree stops the view but not the renderer or TIME.
			_adapter_before_pause = float(_view.stats().get("adapter_ms_last", 0.0))
			paused = true
		442, 443, 444, 445:
			_check_paused_sea()
		446:
			if float(_view.stats().get("adapter_ms_last", -1.0)) != _adapter_before_pause:
				_fail("the view ran a frame while the tree was paused")
			paused = false
		450:
			_expect(report, "wave_set_matches", true)
			_expect(_view.pick(report["own_screen"]), "kind", &"own")


## While the tree is paused the ocean unit does not run, so the clock node keeps the wave reference time at
## its replica of TIME. This script's _process runs before the nodes', so both are last frame's values.
## The headless renderer keeps no global parameters and answers null; the comparison then only runs in a window.
func _check_paused_sea() -> void:
	var clock: Node = root.get_node_or_null(^"BpShaderClock")
	if clock == null:
		_fail("no shader clock is in the tree")
		return
	var t_ref: Variant = RenderingServer.global_shader_parameter_get(&"bp_waves_t_ref")
	if t_ref is float:
		_paused_sea_checked += 1
		var replica: float = clock.call(&"shader_time")
		if absf(float(t_ref) - replica) > 1.0e-4:
			_fail("the sea runs on under a paused tree: t_ref %f, TIME replica %f" % [float(t_ref), replica])


func _final_checks() -> void:
	var report: Dictionary = _check.report(_view)
	print("world check: ", report)
	print("world check: stats ", _view.stats(), " signals ", _counts, " re-entered ", _reentered, " paused-sea readbacks ", _paused_sea_checked)
	_expect(report, "scene", true)
	_expect(report, "clock_running", true)
	_expect(report, "clock_calibrated", true)
	if float(report.get("sea_height_error_m", 1.0)) > 0.001:
		_fail("the renderer's sea differs from the simulation's by %f m" % float(report["sea_height_error_m"]))
	if _min_above_sea < 1.0 - 0.001:
		_fail("the camera came within %f m of the sea" % _min_above_sea)
	if (report["origin"] as Vector2).x < 8000.0:
		_fail("the origin did not follow the 9 km move: %s" % [report["origin"]])
	if not _held_seen:
		_fail("the aim readout was never checked")
	if _counts[&"warmed"] != 1:
		_fail("warmed_up was emitted %d times" % _counts[&"warmed"])
	if _counts[&"rebased"] < 1:
		_fail("origin_rebased was never emitted")
	if _counts[&"environment"] < 2:
		_fail("environment_changed was emitted %d times" % _counts[&"environment"])
	if _counts[&"view"] != 2:
		_fail("view_changed was emitted %d times" % _counts[&"view"])
	if _reentered < 5:
		_fail("signal handlers called back %d times" % _reentered)
	if FileAccess.file_exists("res://art/gen/lut_dusk_clear_17.bin") and _view.lut() == null:
		_fail("the LUT files exist but lut() is null")
	if float(_view.stats().get("adapter_ms_p99", 0.0)) <= 0.0:
		_fail("no adapter time was measured")


# --- the stage (captures only) ---

func _color(entry: Dictionary) -> Color:
	var lin: Array = entry["linear_rgb"]
	return Color(lin[0], lin[1], lin[2]).linear_to_srgb()


func _add_stage() -> void:
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(TOKENS))
	_tokens = parsed if parsed is Dictionary else {}
	_sky = ShaderMaterial.new()
	_sky.shader = load("res://shaders/sky.gdshader")
	var sky := Sky.new()
	sky.sky_material = _sky
	sky.process_mode = Sky.PROCESS_MODE_INCREMENTAL
	_env = Environment.new()
	_env.background_mode = Environment.BG_SKY
	_env.sky = sky
	_env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	_env.reflected_light_source = Environment.REFLECTION_SOURCE_SKY
	_env.tonemap_mode = Environment.TONE_MAPPER_AGX
	_env.tonemap_agx_white = 8.0
	_env.tonemap_agx_contrast = 1.25
	_env.fog_enabled = true
	_env.fog_height_density = 0.0
	_env.fog_sky_affect = 0.0
	var world := WorldEnvironment.new()
	world.environment = _env
	root.add_child(world)
	_sun = DirectionalLight3D.new()
	_sun.shadow_enabled = true
	_sun.directional_shadow_max_distance = 500.0
	_sun.light_angular_distance = 0.5
	root.add_child(_sun)
	var box := BoxMesh.new()
	# Posts centred on the surface: every one must show the same 1.5 m above the water.
	box.size = Vector3(0.4, 3.0, 0.4)
	for i: int in 25:
		var marker := MeshInstance3D.new()
		marker.mesh = box
		marker.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
		root.add_child(marker)
		_floats.append(marker)
	var plate_shader := Shader.new()
	plate_shader.code = PLATE_SHADER
	var plate_mesh := BoxMesh.new()
	plate_mesh.size = Vector3(3.0, 0.2, 3.0)
	for tolerance: float in PLATE_TOLERANCES_M:
		var material := ShaderMaterial.new()
		material.shader = plate_shader
		material.set_shader_parameter(&"lift", PLATE_LIFT_M)
		material.set_shader_parameter(&"tolerance", tolerance)
		var plate := MeshInstance3D.new()
		plate.mesh = plate_mesh
		plate.material_override = material
		plate.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
		root.add_child(plate)
		_plates.append(plate)


func _place_floats() -> void:
	if _floats.is_empty():
		return
	var probes: PackedVector3Array = _check.sea_probes()
	for i: int in mini(probes.size(), _floats.size()):
		_floats[i].position = probes[i]
	# The plates ride above three of the nearer probes.
	for i: int in _plates.size():
		_plates[i].position = probes[6 * i + 1] + Vector3(0.0, PLATE_LIFT_M, 0.0)


## The GPU residual (L1893): each plate is white where the wave include, fed this frame's uniforms and
## reference time, puts the surface within the plate's tolerance of the simulation's height.
func _read_plates(when: String) -> void:
	if _plates.is_empty():
		return
	var image: Image = root.get_texture().get_image()
	var scale: Vector2 = Vector2(image.get_size()) / root.get_visible_rect().size
	var passed: Array[String] = []
	for i: int in _plates.size():
		var screen: Vector2 = _camera.unproject_position(_plates[i].position)
		var pixel := Vector2i(screen * scale)
		if _camera.is_position_behind(_plates[i].position) or pixel.x < 0 or pixel.y < 0 or pixel.x >= image.get_width() or pixel.y >= image.get_height():
			passed.append("%s mm: off screen" % [PLATE_TOLERANCES_M[i] * 1000.0])
			continue
		var white: bool = image.get_pixelv(pixel).get_luminance() > 0.5
		passed.append("%s mm: %s" % [PLATE_TOLERANCES_M[i] * 1000.0, "within" if white else "OUTSIDE"])
		if not white and PLATE_TOLERANCES_M[i] >= 0.01:
			_fail("the drawn sea is more than 1 cm from the simulation's (%s)" % when)
	print("world check: GPU wave residual ", when, ": ", passed)


func _apply_environment(env: Dictionary) -> void:
	if _env == null or _tokens.is_empty():
		return
	var sun_dir: Vector3 = env["sun_dir"]
	_sun.look_at_from_position(Vector3.ZERO, -sun_dir, Vector3.UP)
	var weather_name: String = String(env["preset_to"]).trim_prefix("dusk_")
	for entry: Dictionary in _tokens.get("sky", []):
		if entry.get("time", "") == "dusk":
			_sky.set_shader_parameter(&"zenith_color", _color(entry["zenith"]))
			_sky.set_shader_parameter(&"horizon_color", _color(entry["horizon"]))
			_sky.set_shader_parameter(&"cirrus_color", _color(entry["horizon"]).lerp(_color(entry["key_light"]), 0.6))
			_sun.light_color = _color(entry["key_light"])
	for entry: Dictionary in _tokens.get("sea", []):
		if entry.get("token", "") == "sea.deep":
			_sky.set_shader_parameter(&"ground_color", _color(entry))
	for entry: Dictionary in _tokens.get("fog", []):
		if entry.get("time", "") == "dusk" and entry.get("weather", "") == weather_name:
			_env.fog_light_color = _color(entry)
			_sky.set_shader_parameter(&"overcast_color", _color(entry))
	var weather: Dictionary = env["weather"]
	_sky.set_shader_parameter(&"overcast", float(weather["cloud"]) * float(env["blend"]))
	_sky.set_shader_parameter(&"sun_glow", 0.5)
	_sun.light_energy = lerpf(3.0, 1.2, float(weather["cloud"]))
	_env.fog_density = env["fog_density"]
	_env.fog_aerial_perspective = env["aerial_perspective"]
	_env.fog_sun_scatter = env["fog_sun_scatter"]
	var lut: Texture3D = _view.lut()
	if lut != null:
		_env.adjustment_enabled = true
		_env.adjustment_color_correction = lut


func _shot(suffix: String) -> void:
	if _capture == "":
		return
	root.get_texture().get_image().save_png("%s_%s.png" % [_capture, suffix])


# --- reporting ---

func _fail(message: String) -> void:
	_failures += 1
	push_error("world check: frame %d: %s" % [_frame, message])


func _expect(report: Dictionary, key: String, want: Variant) -> void:
	if report.get(key) != want:
		_fail("%s is %s, expected %s" % [key, report.get(key), want])
