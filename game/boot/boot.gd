extends Node
## Main scene. Unit: ui-boot. DESIGN.md L1270 (startup log), L1334 (consent before anything is sent),
## L1739 (first-launch accessibility flags), L2016 (quality auto-detect), L1833 (precompile screen).
## The smoke test (`godot --headless --path . -- --smoke`) must keep printing the version and quit
## with no error line.
##
## Order of a normal launch:
## 1. Print the version (the smoke test stops here and quits).
## 2. Log what the renderer started as, with every fallback (`App.log_startup`); in safe mode the
##    persistent update-your-driver notice goes up.
## 3. `Settings.reload()`, then `Ui.refresh()`. On the first launch the OS accessibility flags set
##    reduce motion with blink-off, reduce transparency and high contrast (L1739).
## 4. Crash-report consent while it has never been asked (L1334); reporting starts only after Send.
## 5. Quality auto-detect: map the machine to a tier, and on the first launch or after an adapter or
##    driver change run the scripted flotilla and the two A/B tests (L2016-2020).
## 6. Shader warm-up behind the precompile screen (L1833, L1358).
## 7. Hand over to `Screens` routing and open the main menu, or what a flag asks for: `--benchmark`
##    opens the benchmark, `--replay <file>` plays a replay, `--join <code>` joins a lobby.
## 8. If the previous run crashed (`App.previous_run_crashed`), a message names the logs folder.
## A headless run (CI, a dedicated test) skips every screen that needs a player or a GPU.
## The flow runs only when this scene is the main scene; the compile check instantiates it inert.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

const ConsentScreen := preload("res://boot/consent.gd")
const WORLD_SCENE := "res://world/world.tscn"
const WARMUP_SCENE := "res://world/warmup.tscn"
## Scripted flotilla (L2018): 10 s of a fixed sandbox, then 2 s per A/B arm.
const FLOTILLA_S := 10.0
const AB_ARM_S := 2.0
## Frames discarded after each switch, so a pipeline compile or a resize is not measured.
const SETTLE_FRAMES := 10
## How long the flotilla may take to load before the check gives up and keeps the mapped tier.
const FLOTILLA_LOAD_LIMIT_S := 60.0
## How long the warm-up may take before the precompile screen lifts anyway.
const WARMUP_LIMIT_S := 90.0
const FLOTILLA_SEED := 2016
## Bp constants the flow needs, kept here so the script parses before the extension loads.
const NONE := -1
const SESSION_IN_SORTIE := 3
const SESSION_FAILED := 6
const TIME_DUSK := 1
const WEATHER_CLEAR := 0
const FLOTILLA_SEA_STATE := 3
const PRIVACY_NOT_ASKED := -1
const PRIVACY_SEND := 1

var _frame_ms: PackedFloat32Array = []
var _gpu_ms: PackedFloat32Array = []


func _ready() -> void:
	var probe := BpProbe.new()
	print("gdclient ", probe.version())
	probe.free()
	if "--smoke" in OS.get_cmdline_user_args():
		get_tree().quit()
		return
	if get_tree().current_scene != self:
		return
	_run.call_deferred()


func _run() -> void:
	var cmd: Dictionary = App.command_line()
	var safe := App.log_startup(startup_report())
	Screens.set_safe_mode(safe)
	var first := Settings.reload()
	Ui.refresh()
	if first or not _flag(Settings.get_value(&"access.first_launch_done")):
		Settings.apply_os_accessibility(
			DisplayServer.accessibility_should_reduce_animation() == 1,
			DisplayServer.accessibility_should_reduce_transparency() == 1,
			DisplayServer.accessibility_should_increase_contrast() == 1)
		Ui.refresh()
	var headless := DisplayServer.get_name() == "headless"
	var automated := not String(cmd.get("scripted", "")).is_empty() or not String(cmd.get("capture", "")).is_empty()
	var privacy := _number(Settings.get_value(&"privacy.crash_reports"), PRIVACY_NOT_ASKED)
	if privacy == PRIVACY_NOT_ASKED and not headless and not automated:
		await _ask_consent()
	elif privacy == PRIVACY_SEND:
		ConsentScreen.start_reporting()
	await _detect_quality(first, safe, headless or automated)
	if privacy == PRIVACY_SEND or _number(Settings.get_value(&"privacy.crash_reports"), PRIVACY_NOT_ASKED) == PRIVACY_SEND:
		# The tier is known only now; tag it in both SDKs (L1334 lists it in every report).
		App.crash_reports_set_tag("tier", str(_number(Quality.profile().get("tier", NONE), NONE)))
	if not headless:
		await _warm_shaders()
	Screens.start_routing()
	_route(cmd)
	if not headless and not automated:
		_tell_about_crash()
	queue_free()


## The previous run crashed or was killed: name the folder that holds its log, so a tester can attach
## it whether or not crash reports are on.
func _tell_about_crash() -> void:
	if not App.previous_run_crashed():
		return
	var body := tr(&"settings_ui.playtest.crash_body").format([App.log_folder()])
	Screens.open_message(&"settings_ui.playtest.crash_title", body)


## A setting read as a flag; anything that is not a true bool or a non-zero number is false.
static func _flag(v: Variant) -> bool:
	if v is bool:
		return v
	if v is int or v is float:
		return v != 0
	return false


static func _number(v: Variant, fallback: int) -> int:
	if v is int or v is float:
		return int(v)
	return fallback


# --- 2. startup log --------------------------------------------------------------------------------

## What the renderer started as and every fallback from what the project asked for (L1270), in
## the shape `App.log_startup` takes.
static func startup_report() -> Dictionary:
	var method := RenderingServer.get_current_rendering_method()
	var driver := RenderingServer.get_current_rendering_driver_name()
	var fallbacks: PackedStringArray = []
	var wanted_method := _requested("--rendering-method", "rendering/renderer/rendering_method")
	if not wanted_method.is_empty() and wanted_method != method:
		fallbacks.append("rendering method %s -> %s" % [wanted_method, method])
	var driver_key := "rendering/gl_compatibility/driver" if method == "gl_compatibility" else "rendering/rendering_device/driver"
	var wanted_driver := _requested("--rendering-driver", driver_key)
	if wanted_method == method and not wanted_driver.is_empty() and wanted_driver != driver:
		fallbacks.append("driver %s -> %s" % [wanted_driver, driver])
	return {
		"rendering_method": method,
		"driver": driver,
		"adapter": RenderingServer.get_video_adapter_name(),
		"api_version": RenderingServer.get_video_adapter_api_version(),
		"fallbacks": fallbacks,
	}


## What was asked for: a command-line override, else the project setting for this platform.
static func _requested(flag: String, setting: String) -> String:
	var args := OS.get_cmdline_args()
	for i in args.size():
		var a := args[i]
		if a == flag and i + 1 < args.size():
			return args[i + 1]
		if a.begins_with(flag + "="):
			return a.substr(flag.length() + 1)
	return String(ProjectSettings.get_setting_with_override(setting))


# --- 4. consent ----------------------------------------------------------------------------------

func _ask_consent() -> void:
	var screen := Screens.change(&"consent")
	if screen == null or not screen.has_signal(&"decided"):
		push_error("boot: the consent screen did not open; crash reports stay off")
		return
	await Signal(screen, &"decided")


# --- 5. quality auto-detect ----------------------------------------------------------------------

## The machine description `Quality.detect` maps to a tier (L2016 step 1).
static func machine() -> Dictionary:
	var memory := OS.get_memory_info()
	var driver_info := OS.get_video_adapter_driver_info()
	return {
		"adapter": RenderingServer.get_video_adapter_name(),
		"vendor": RenderingServer.get_video_adapter_vendor(),
		"driver_version": driver_info[1] if driver_info.size() > 1 else "",
		"api_version": RenderingServer.get_video_adapter_api_version(),
		"processor": OS.get_processor_name(),
		"model": OS.get_model_name(),
		"memory_gb": float(memory.get("physical", 0)) / 1073741824.0,
		"rendering_method": RenderingServer.get_current_rendering_method(),
		"window_px": DisplayServer.window_get_size(),
	}


func _detect_quality(first: bool, safe: bool, unattended: bool) -> void:
	Quality.detect(machine())
	var result: Dictionary = Quality.detect_result()
	var due := first or bool(result.get("changed_since_last", false))
	# Safe mode has no tiers to choose between, and a run without a GPU or a player measures nothing.
	if not due or safe or unattended:
		return
	await _run_flotilla()


## L2018 step 3: a 10 s scripted flotilla that also warms pipelines, then 2 s A/Bs of MSAA 2x against
## SMAA 1x and of 4 against 2 shadow splits. GPU time where the driver measures it; on a Mac, frame
## time with V-Sync off. Gives up, keeping the mapped tier, if the flotilla cannot start.
func _run_flotilla() -> void:
	var setup := _flotilla_setup()
	if setup.is_empty():
		push_warning("boot: no starter hull for the graphics check; keeping the mapped tier")
		return
	Screens.clear()
	Screens.show_caption(&"boot.quality.loading", 0.0)
	# The check's battle is not the player's: its guns, alarms and radio stay unheard under the cover.
	Sound.hold_world(true)
	# `start_sandbox` returns `Array[MsgDict]` of problems; any problem means nothing is loading.
	var problems: Array = Session.start_sandbox(setup)
	if not problems.is_empty():
		var first_problem: Variant = problems[0]
		push_warning("boot: the graphics check could not start: %s" % _Text.resolve(first_problem))
		Sound.hold_world(false)
		Screens.hide_loading()
		return
	var world := (load(WORLD_SCENE) as PackedScene).instantiate()
	add_child(world)
	var waited := 0.0
	while int(Session.state()) != SESSION_IN_SORTIE:
		if int(Session.state()) == SESSION_FAILED or waited > FLOTILLA_LOAD_LIMIT_S:
			push_warning("boot: the graphics check did not load; keeping the mapped tier")
			_end_flotilla(world)
			return
		await get_tree().process_frame
		waited += get_process_delta_time()
	var viewport := get_viewport()
	var rid := viewport.get_viewport_rid()
	RenderingServer.viewport_set_measure_render_time(rid, true)
	var vsync := DisplayServer.window_get_vsync_mode()
	var mac := OS.get_name() == "macOS"
	var fps_cap := Engine.max_fps
	if mac:
		# Frame time stands in for GPU time on Metal, so nothing may cap it (L2018).
		DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED)
		Engine.max_fps = 0
	var msaa := viewport.msaa_3d
	var ssaa := viewport.screen_space_aa
	var sun := _find_sun(world)
	var splits := sun.directional_shadow_mode if sun != null else DirectionalLight3D.SHADOW_PARALLEL_4_SPLITS
	var total := FLOTILLA_S + 4.0 * AB_ARM_S
	Screens.show_caption(&"boot.quality.measuring", 0.0)
	await _measure(FLOTILLA_S, 0.0, total, rid)
	var p95 := percentile(_frame_ms, 0.95)
	var gpu_p95 := percentile(_gpu_ms, 0.95)
	viewport.msaa_3d = Viewport.MSAA_2X
	viewport.screen_space_aa = Viewport.SCREEN_SPACE_AA_DISABLED
	var msaa_cost := await _arm(FLOTILLA_S, total, rid, mac)
	viewport.msaa_3d = Viewport.MSAA_DISABLED
	viewport.screen_space_aa = Viewport.SCREEN_SPACE_AA_SMAA
	var smaa_cost := await _arm(FLOTILLA_S + AB_ARM_S, total, rid, mac)
	viewport.msaa_3d = msaa
	viewport.screen_space_aa = ssaa
	var four_cost := -1.0
	var two_cost := -1.0
	if sun != null:
		sun.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_4_SPLITS
		four_cost = await _arm(FLOTILLA_S + 2.0 * AB_ARM_S, total, rid, mac)
		sun.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_2_SPLITS
		two_cost = await _arm(FLOTILLA_S + 3.0 * AB_ARM_S, total, rid, mac)
		sun.directional_shadow_mode = splits
	if mac:
		DisplayServer.window_set_vsync_mode(vsync)
		Engine.max_fps = fps_cap
	RenderingServer.viewport_set_measure_render_time(rid, false)
	# msaa_ms and four_splits_ms are what each costs over its cheaper arm (L2018: SMAA when MSAA
	# costs over 1.0 ms, 2 splits when 4 cost over 2.0 ms). Absolute arm times ride along.
	Quality.detect_timings({
		"p95_ms": p95,
		"gpu_p95_ms": gpu_p95 if gpu_p95 > 0.0 else -1.0,
		"msaa_ms": maxf(0.0, msaa_cost - smaa_cost),
		"four_splits_ms": maxf(0.0, four_cost - two_cost) if sun != null else 0.0,
		"msaa_arm_ms": msaa_cost,
		"smaa_arm_ms": smaa_cost,
		"four_splits_arm_ms": four_cost,
		"two_splits_arm_ms": two_cost,
	})
	_end_flotilla(world)


func _flotilla_setup() -> Dictionary:
	var starters: Array = Content.starters(NONE)
	var paths: PackedStringArray = []
	for s: Variant in starters:
		if s is Dictionary and not String((s as Dictionary).get("path", "")).is_empty():
			paths.append(String((s as Dictionary).get("path", "")))
	if paths.is_empty():
		return {}
	var targets: Array = []
	for i in range(1, paths.size()):
		targets.append({"blueprint": paths[i], "hostile": i % 2 == 1, "count": 1})
	if targets.is_empty():
		targets.append({"blueprint": paths[0], "hostile": true, "count": 3})
	return {
		"blueprint": paths[0],
		"weather": WEATHER_CLEAR,
		"sea_state": FLOTILLA_SEA_STATE,
		"time_of_day": TIME_DUSK,
		"seed": FLOTILLA_SEED,
		"targets": targets,
	}


## One A/B arm: settle, then the p95 of GPU time where measured, else frame time.
func _arm(start_s: float, total_s: float, rid: RID, mac: bool) -> float:
	await _measure(AB_ARM_S, start_s, total_s, rid)
	var gpu := percentile(_gpu_ms, 0.95)
	if not mac and gpu > 0.0:
		return gpu
	return percentile(_frame_ms, 0.95)


func _measure(seconds: float, start_s: float, total_s: float, rid: RID) -> void:
	_frame_ms.clear()
	_gpu_ms.clear()
	for i in SETTLE_FRAMES:
		await get_tree().process_frame
	var elapsed := 0.0
	while elapsed < seconds:
		await get_tree().process_frame
		var dt := get_process_delta_time()
		elapsed += dt
		_frame_ms.append(dt * 1000.0)
		var gpu := RenderingServer.viewport_get_measured_render_time_gpu(rid)
		if gpu > 0.0:
			_gpu_ms.append(gpu)
		Screens.set_loading_progress((start_s + elapsed) / total_s)


func _end_flotilla(world: Node) -> void:
	Session.leave(false)
	Sound.hold_world(false)
	if is_instance_valid(world):
		world.queue_free()
	Screens.hide_loading()


static func _find_sun(root: Node) -> DirectionalLight3D:
	if root is DirectionalLight3D:
		return root
	for child: Node in root.get_children():
		var found := _find_sun(child)
		if found != null:
			return found
	return null


## Nearest-rank percentile of `samples` (`q` 0..1); -1.0 for no samples.
static func percentile(samples: PackedFloat32Array, q: float) -> float:
	if samples.is_empty():
		return -1.0
	var sorted := samples.duplicate()
	sorted.sort()
	var rank := clampi(ceili(clampf(q, 0.0, 1.0) * sorted.size()) - 1, 0, sorted.size() - 1)
	return sorted[rank]


# --- 6. shader warm-up ---------------------------------------------------------------------------

## Draw every material and effect once behind the precompile screen (L1833, L1358).
func _warm_shaders() -> void:
	if not ResourceLoader.exists(WARMUP_SCENE):
		return
	var warmup := (load(WARMUP_SCENE) as PackedScene).instantiate()
	var view := _find_view(warmup)
	if view == null:
		# The warm-up scene has nothing to draw yet.
		warmup.free()
		return
	Screens.clear()
	Screens.show_loading(&"boot.warmup.status", -1.0)
	var done := [false]
	view.connect(&"warmed_up", func() -> void: done[0] = true, CONNECT_ONE_SHOT)
	add_child(warmup)
	var waited := 0.0
	while not done[0] and waited < WARMUP_LIMIT_S:
		await get_tree().process_frame
		waited += get_process_delta_time()
	if not done[0]:
		push_warning("boot: shader warm-up did not finish in %d s" % int(WARMUP_LIMIT_S))
	warmup.queue_free()
	Screens.hide_loading()


static func _find_view(root: Node) -> Node:
	if root.is_class("BpWorldView"):
		return root
	for child: Node in root.get_children():
		var found := _find_view(child)
		if found != null:
			return found
	return null


# --- 7. first screen -----------------------------------------------------------------------------

func _route(cmd: Dictionary) -> void:
	if bool(cmd.get("benchmark", false)):
		Screens.change(&"benchmark", {"start": true})
		return
	var replay := String(cmd.get("replay", ""))
	if not replay.is_empty():
		Screens.change(&"main_menu")
		# A replay this build cannot open reports through Session.failed, which Screens shows.
		Session.replay_open(replay)
		return
	var join := String(cmd.get("join", ""))
	if not join.is_empty():
		Screens.change(&"multiplayer", {"join_code": join})
		if not Lobby.join_code(join):
			Screens.open_message(&"boot.join.failed_title", &"boot.join.failed_body")
		return
	Screens.change(&"main_menu")
