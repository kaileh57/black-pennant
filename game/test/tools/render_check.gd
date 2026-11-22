extends SceneTree
## In-engine check of the hull, part and projectile renderers (Rust `BpRenderCheck`):
##   godot --headless --path . -s res://test/tools/render_check.gd
## Runs a scripted scene for 120 frames, checks the renderers' counters and exits 1 on any failure.
## With a window instead of --headless, and `-- --capture=<path.png>` after the script, it also adds a
## camera and a sun and saves one frame, so the meshes can be looked at.

const FRAMES := 120

var _check: BpRenderCheck
var _frame := 0
var _failures := 0
var _capture := ""


func _initialize() -> void:
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--capture="):
			_capture = arg.trim_prefix("--capture=")
	_check = BpRenderCheck.new()
	_check.start(root.get_world_3d().scenario)
	_check.show_dock_parts()
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
	sun.look_at_from_position(Vector3(60.0, 25.0, -80.0), Vector3.ZERO)


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame <= FRAMES:
		_check.step(300)
		return false
	var report: Dictionary = _check.report()
	print("render check: ", report)
	_expect(report, "near_lod", 0)
	_expect(report, "bounds", true)
	_expect(report, "ray_hit", true)
	_expect(report, "markings", 2)
	_expect(report, "part_mesh", true)
	_expect(report, "mask_texture", true)
	_expect(report, "uploads_pending", 0)
	_expect(report, "shell_ends", 1)
	_at_least(report, "far_lod", 1)
	_at_least(report, "chunks_drawn", 2)
	_at_least(report, "part_instances", 10)
	_at_least(report, "muzzles", 2)
	_at_least(report, "projectile_instances", 6)
	if _capture != "":
		root.get_texture().get_image().save_png(_capture)
	_check.finish()
	_check = null
	quit(1 if _failures > 0 else 0)
	return true


func _expect(report: Dictionary, key: String, want: Variant) -> void:
	if report.get(key) != want:
		_failures += 1
		push_error("render check: %s is %s, expected %s" % [key, report.get(key), want])


func _at_least(report: Dictionary, key: String, least: int) -> void:
	if int(report.get(key, -1)) < least:
		_failures += 1
		push_error("render check: %s is %s, expected at least %d" % [key, report.get(key), least])
