extends Node
## Windowed look check of the sortie world and the dry-dock stage (units world-scene, world-dock): a real
## skirmish on the real sea through res://world/world.tscn, or a starter design in res://world/dock_stage.tscn,
## then one PNG per shot. Prints the rendering driver and the applied quality profile.
##   godot --path game --resolution 1600x900 res://test/capture/look_capture.tscn -- --out=/abs/dir [options]
## Options: --mode=sortie|dock (default sortie), --name=NAME (file stem), --weather=N (Bp.WEATHER_*),
## --time=N (Bp.TIME_*), --sea=N, --seed=N, --bp=starters/x.bp.ron, --frames=N (sortie frames before the
## first shot), --orbit=yaw,pitch (degrees added to the opening orbit), --zoom=steps, --tier=0|1|2,
## --plain (rule A0: glow, SSAO, SSR and volumetric fog off), --shots=N (extra shots every --every frames),
## --frame=all|bow|stern|top|below (dock framing), --blur=0..1 (dock depth of field), --spin=deg (orbit
## yaw added after each sortie shot), --lightning (shoot the frame after the first lightning pulse),
## --speed=kn (ordered speed, default 18), --turn=N (sortie frame of a 90 degree turn to starboard),
## --dist=m (orbit distance, overrides the opening framing), --view=plot (the chart table),
## --fire=N (from sortie frame N, every own gun fires along the bow every 90 frames), --perf=N (V-Sync and the
## frame cap off for the N frames before the first shot; prints frame-time and GPU-time p50, p95 and p99).
## Headless, and when instanced by check_project, it does nothing.

var _out := "user://captures"
var _mode := "sortie"
var _name := ""
var _weather := 0
var _time := 1
var _sea := 3
var _seed := 4242
var _bp := ""
var _frames := 240
var _orbit := Vector2.ZERO
var _zoom := 0.0
var _tier := -1
var _plain := false
var _shots := 1
var _every := 120
var _frame_what := "all"
var _blur := 0.0
var _spin := 0.0
var _lightning := false
var _glow_pair := false
var _overlays := 0
var _slice := -1
var _thumb := false
var _flash_at := -1
var _speed := 18.0
var _turn := -1
var _dist := 0.0
var _view_plot := false
var _fire := -1
var _perf := 0
var _frame_us: PackedFloat64Array = []
var _gpu_ms: PackedFloat64Array = []
var _last_us := 0
var _check: BpSessionCheck

var _root: Node3D
var _frame := 0
var _start := -1
var _taken := 0
var _active := false
var _shown := false


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		var kv := arg.trim_prefix("--").split("=", true, 1)
		var v: String = kv[1] if kv.size() > 1 else ""
		match kv[0]:
			"out": _out = v
			"mode": _mode = v
			"name": _name = v
			"weather": _weather = int(v)
			"time": _time = int(v)
			"sea": _sea = int(v)
			"seed": _seed = int(v)
			"bp": _bp = v
			"frames": _frames = int(v)
			"orbit":
				var p := v.split(",")
				_orbit = Vector2(float(p[0]), float(p[1]) if p.size() > 1 else 0.0)
			"zoom": _zoom = float(v)
			"tier": _tier = int(v)
			"plain": _plain = true
			"shots": _shots = int(v)
			"every": _every = int(v)
			"frame": _frame_what = v
			"blur": _blur = float(v)
			"spin": _spin = float(v)
			"lightning": _lightning = true
			"glow-pair": _glow_pair = true
			"overlays": _overlays = int(v)
			"slice": _slice = int(v)
			"thumb": _thumb = true
			"speed": _speed = float(v)
			"turn": _turn = int(v)
			"dist": _dist = float(v)
			"view": _view_plot = v == "plot"
			"fire": _fire = int(v)
			"perf": _perf = int(v)
	if _name == "":
		_name = "%s_t%d_w%d" % [_mode, _time, _weather]
	DirAccess.make_dir_recursive_absolute(_out)
	# Godot skips drawing while its window is occluded, and every later shot then saves the same stale frame:
	# keep the capture window above the others.
	DisplayServer.window_set_flag(DisplayServer.WINDOW_FLAG_ALWAYS_ON_TOP, true)
	print("look_capture: driver ", RenderingServer.get_current_rendering_driver_name(), " method ", RenderingServer.get_current_rendering_method())
	if RenderingServer.get_current_rendering_method() == "gl_compatibility":
		# Boot's auto-detect is what puts the Compatibility renderer on the Fallback tier.
		Quality.detect((load("res://boot/boot.gd") as GDScript).call(&"machine"))
	if _tier >= 0:
		Settings.set_value(&"graphics.tier", _tier)
	Settings.set_value(&"access.blur", _blur)
	if _mode == "dock":
		_start_dock()
	else:
		_start_sortie()


func _start_sortie() -> void:
	_root = (load("res://world/world.tscn") as PackedScene).instantiate() as Node3D
	add_child(_root)
	var problems := Session.start_skirmish({
		"template": Bp.MISSION_CONVOY_RAID, "act": Bp.ACT_3, "enemy_per_player": 2, "difficulty": {"preset": "normal"},
		"weather": _weather, "sea_state": _sea, "time_of_day": _time, "seed": _seed,
		"blueprint": _bp if _bp != "" else "starters/corvette60.bp.ron",
	})
	if not problems.is_empty():
		push_error("look_capture: start_skirmish refused: " + str(problems))
		get_tree().quit(1)
		return
	_active = true


func _start_dock() -> void:
	_root = (load("res://world/dock_stage.tscn") as PackedScene).instantiate() as Node3D
	add_child(_root)
	var view := _root.get_node(^"View") as BpEditorView
	view.design_shown.connect(func(_rev: int) -> void: _shown = true)
	var issues: Array = Editor.open(Bp.DOCK_SANDBOX, {"blueprint": _bp if _bp != "" else "starters/destroyer90.bp.ron"})
	for i: Dictionary in issues:
		if bool(i["fatal"]):
			push_error("look_capture: open refused: " + str(issues))
			get_tree().quit(1)
			return
	_active = true


func _process(_delta: float) -> void:
	if not _active:
		return
	_frame += 1
	if _frame > 6000:
		push_error("look_capture: timed out")
		_end(1)
		return
	if _mode == "dock":
		_dock_step()
	else:
		_sortie_step()


func _sortie_step() -> void:
	if _start < 0:
		if Session.state() == Bp.SESSION_IN_SORTIE:
			_start = _frame
			_apply_plain()
		elif Session.state() == Bp.SESSION_FAILED:
			push_error("look_capture: the session failed")
			_end(1)
		return
	var since := _frame - _start
	var view := _root.get_node(^"View") as BpWorldView
	if _lightning:
		if since == 1:
			view.lightning.connect(func(i: float, d: float, b: float) -> void:
				print("look_capture: lightning ", i, " ", d, " ", b)
				if _flash_at < 0:
					_flash_at = _frame + 1)
		if _flash_at > 0 and _frame == _flash_at:
			_shoot()
			_end(0)
		if since == 20:
			view.orbit(_orbit.x, _orbit.y)
		return
	if since == 20:
		view.orbit(_orbit.x, _orbit.y)
		if _zoom != 0.0:
			view.zoom(_zoom)
		if _dist > 0.0:
			var now := float(view.orbit_state()["distance_m"])
			view.zoom(log(_dist / now) / log(0.88))
		if _view_plot:
			view.set_view(Bp.VIEW_PLOT)
		print("look_capture: orbit ", view.orbit_state())
	if since == 30:
		_check = BpSessionCheck.new()
		_course(25.0)
	if _turn > 0 and since == _turn:
		_course(90.0)
	if _fire > 0 and since >= _fire and (since - _fire) % 90 == 0:
		var facts := _check.facts()
		var guns: PackedInt32Array = facts.get("guns", PackedInt32Array())
		if guns.is_empty():
			guns = facts.get("mounts", PackedInt32Array())
		for slot: int in guns:
			if since == _fire:
				_check.take_mount(slot, true)
			else:
				_check.fire_mount(slot, 0)
	if _perf > 0:
		_perf_step(since)
	if _glow_pair and since == _frames + 1:
		var genv := (_root.get_node(^"Environment") as WorldEnvironment).environment
		_shoot_named("%s_noglow" % _name)
		genv.glow_enabled = true
		_end(0)
		return
	if since >= _frames and (since - _frames) % _every == 0:
		_shoot()
		if _glow_pair:
			(_root.get_node(^"Environment") as WorldEnvironment).environment.glow_enabled = false
			return
		if _spin != 0.0:
			view.orbit(_spin, 0.0)
		if _taken >= _shots:
			var env := (_root.get_node(^"Environment") as WorldEnvironment).environment
			print("look_capture: lut ", env.adjustment_enabled, " ", env.adjustment_color_correction, " exposure ", env.tonemap_exposure, " ambient ", env.ambient_light_color, " share ", env.ambient_light_sky_contribution)
			print("look_capture: environment ", view.environment())
			print("look_capture: profile ", (_root.get_node(^"Profile")).call(&"applied"))
			print("look_capture: stats ", view.stats())
			if _check != null and _fire > 0:
				print("look_capture: facts ", _check.facts())
			_end(0)


func _dock_step() -> void:
	if _start < 0:
		if _shown or _frame > 1200:
			_start = _frame
			_apply_plain()
			var view := _root.get_node(^"View") as BpEditorView
			view.frame(StringName(_frame_what))
			if _orbit != Vector2.ZERO:
				view.orbit(_orbit, Vector2(800, 450))
			if _overlays != 0:
				view.set_overlays(_overlays)
			if _slice >= 0:
				view.set_deck_slice(_slice)
		return
	if _frame - _start == 150:
		_shoot()
		if _thumb:
			var img: Image = (_root.get_node(^"View") as BpEditorView).thumbnail(Vector2i(512, 288))
			if img != null:
				img.save_png(_out.path_join("%s_thumb.png" % _name))
				print("look_capture: saved thumbnail")
		_end(0)


## Frame timing for --perf: V-Sync and the cap go off 60 frames before the window, then each frame's wall
## time and the viewport's measured GPU time are kept, and summarised on the window's last frame.
func _perf_step(since: int) -> void:
	var vp := get_viewport().get_viewport_rid()
	if since == _frames - _perf - 60:
		DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED)
		Engine.max_fps = 0
		RenderingServer.viewport_set_measure_render_time(vp, true)
	var now := Time.get_ticks_usec()
	if since > _frames - _perf and since <= _frames:
		_frame_us.append(float(now - _last_us))
		_gpu_ms.append(RenderingServer.viewport_get_measured_render_time_gpu(vp))
	_last_us = now
	if since == _frames and not _frame_us.is_empty():
		var f := _frame_us.duplicate()
		f.sort()
		var g := _gpu_ms.duplicate()
		g.sort()
		var q := func(a: PackedFloat64Array, p: float) -> float: return a[mini(int(p * a.size()), a.size() - 1)]
		print("look_capture: perf frames %d frame_ms p50 %.2f p95 %.2f p99 %.2f gpu_ms p50 %.2f p95 %.2f p99 %.2f" % [
			f.size(), q.call(f, 0.5) / 1000.0, q.call(f, 0.95) / 1000.0, q.call(f, 0.99) / 1000.0,
			q.call(g, 0.5), q.call(g, 0.95), q.call(g, 0.99)])
		print("look_capture: perf primitives %d draws %d video_mem_mb %.0f" % [
			RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_TOTAL_PRIMITIVES_IN_FRAME),
			RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_TOTAL_DRAW_CALLS_IN_FRAME),
			RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_VIDEO_MEM_USED) / 1048576.0])


## Order a course `turn_deg` to starboard of the present heading at the capture speed.
func _course(turn_deg: float) -> void:
	var f := _check.facts()
	var heading := deg_to_rad(float(f.get("heading_deg", 0.0)) + turn_deg)
	_check.set_course(Vector2(f.get("pos", Vector2.ZERO)) + Vector2(sin(heading), -cos(heading)) * 3000.0, _speed)


func _apply_plain() -> void:
	if not _plain:
		return
	var env := (_root.get_node(^"Environment") as WorldEnvironment).environment
	env.glow_enabled = false
	env.ssao_enabled = false
	env.ssr_enabled = false
	env.volumetric_fog_enabled = false


func _shoot() -> void:
	_shoot_named(_name + ("" if _shots <= 1 else "_%d" % _taken))


func _shoot_named(stem: String) -> void:
	var image := get_viewport().get_texture().get_image()
	var path := _out.path_join("%s.png" % stem)
	image.save_png(path)
	print("look_capture: saved ", path)
	_taken += 1


func _end(code: int) -> void:
	_active = false
	if _mode == "dock":
		Editor.close()
	else:
		Session.leave(false)
	get_tree().quit(code)
