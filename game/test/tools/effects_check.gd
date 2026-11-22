extends SceneTree
## In-engine check of the VFX, particle and scenery renderers (Rust `BpEffectsCheck`):
##   godot --headless --path . -s res://test/tools/effects_check.gd
## Runs a scripted scene for 300 frames, checks the renderers' counters and exits 1 on any failure.
## With a window instead of --headless, and `-- --capture=<path.png>` after the script, it also adds a
## camera, a low sun and a sky and saves one frame, so the effects can be looked at.

const FRAMES := 300

var _check: BpEffectsCheck
var _frame := 0
var _failures := 0
var _capture := ""


func _initialize() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--capture="):
			_capture = arg.trim_prefix("--capture=")
	_check = BpEffectsCheck.new()
	_check.start(root.get_world_3d().scenario)
	if _capture != "":
		_add_stage()


func _add_stage() -> void:
	var shot: Dictionary = _check.camera()
	var camera := Camera3D.new()
	camera.fov = shot["fov"]
	camera.far = 20000.0
	root.add_child(camera)
	camera.look_at_from_position(shot["position"], shot["target"])
	camera.make_current()
	var sun := DirectionalLight3D.new()
	sun.shadow_enabled = true
	root.add_child(sun)
	sun.look_at_from_position(Vector3(-300.0, 60.0, 400.0), Vector3.ZERO)
	var env := WorldEnvironment.new()
	var environment := Environment.new()
	environment.background_mode = Environment.BG_SKY
	var sky := Sky.new()
	sky.sky_material = PhysicalSkyMaterial.new()
	environment.sky = sky
	environment.fog_enabled = true
	environment.fog_density = 0.0005
	env.environment = environment
	root.add_child(env)


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame <= FRAMES:
		_check.step()
		return false
	var report: Dictionary = _check.report()
	print("effects check: ", report)
	_at_least(report, "max_vfx_instances", 50)
	_at_least(report, "vfx_instances", 20)
	_at_least(report, "max_particles", 20)
	_at_least(report, "max_lights", 1)
	_at_least(report, "env_rev", 2)
	_at_least(report, "clouds", 6)
	_at_least(report, "island_tiles", 4)
	_expect(report, "lut", true)
	# The design fallback puts the muzzle within 3 m of the part's own anchor and bores the same way.
	if float(report.get("muzzle_error_m", 1e9)) > 3.0 or float(report.get("muzzle_dot", -1.0)) < 0.95:
		_failures += 1
		push_error("effects check: muzzle fallback off by %s m, bore dot %s" % [report.get("muzzle_error_m"), report.get("muzzle_dot")])
	if float(report.get("ground_height", 0.0)) < 20.0:
		_failures += 1
		push_error("effects check: ground_height is %s, expected the island's summit" % report.get("ground_height"))
	if _capture != "":
		root.get_texture().get_image().save_png(_capture)
	_check.finish()
	_check = null
	quit(1 if _failures > 0 else 0)
	return true


func _expect(report: Dictionary, key: String, want: Variant) -> void:
	if report.get(key) != want:
		_failures += 1
		push_error("effects check: %s is %s, expected %s" % [key, report.get(key), want])


func _at_least(report: Dictionary, key: String, least: int) -> void:
	if int(report.get(key, -1)) < least:
		_failures += 1
		push_error("effects check: %s is %s, expected at least %d" % [key, report.get(key), least])
