extends SceneTree
## Headless checks of the world look wiring (units world-scene, world-dock): preset blending, the ambient
## floor, night lights, the lightning pulse, the quality profile reaching viewport, lights and environment,
## a tier switch at run time, the warm-up scene reporting `warmed_up`, and the dry-dock stage stepping aside
## for a sortie. Rendering itself is checked windowed by res://test/capture/look_capture.tscn.
##   godot --headless --path . -s res://test/unit/world_look_test.gd
## Exits 1 on any failure. It changes graphics.tier for one step and puts the old value back.

const LIMIT := 600

var _failures := 0
var _frame := 0
var _step := 0
var _world: Node3D
var _warmup: Node3D
var _dock: Node3D
var _warmed := false
var _tier_before: Variant


func _initialize() -> void:
	_world = (load("res://world/world.tscn") as PackedScene).instantiate() as Node3D
	root.add_child(_world)


func _check(ok: bool, what: String) -> void:
	if not ok:
		_failures += 1
		push_error("world_look_test: FAILED " + what)


func _process(_delta: float) -> bool:
	_frame += 1
	if _frame > LIMIT:
		_check(false, "timed out at step %d" % _step)
		return _finish()
	match _step:
		0:
			if _frame < 3:
				return false
			_presets_and_lights()
			_step = 1
		1:
			# Lightning: a pulse lifts ambient and returns to it within its 150 ms.
			var presets: Node = _world.get_node(^"Presets")
			var env := (_world.get_node(^"Environment") as WorldEnvironment).environment
			presets.call(&"apply_static", Bp.TIME_DUSK, Bp.WEATHER_STORM, 200.0)
			var base := env.ambient_light_energy
			presets.call(&"_on_lightning", 1.0, 0.12, 90.0)
			_check(env.ambient_light_energy > base * 1.5, "lightning lifts ambient (%f over %f)" % [env.ambient_light_energy, base])
			_step = 2
			_step_frame(30)
		2:
			if not _waited():
				return false
			var env := (_world.get_node(^"Environment") as WorldEnvironment).environment
			var sky := env.sky.sky_material as ShaderMaterial
			_check(is_equal_approx(env.ambient_light_energy, 1.0), "ambient back after the pulse (%f)" % env.ambient_light_energy)
			_check((sky.get_shader_parameter(&"lightning") as Vector4).w == 0.0, "sky flash cleared")
			_quality_profile()
			_tier_before = root.get_node(^"Settings").get_value(&"graphics.tier")
			root.get_node(^"Settings").set_value(&"graphics.tier", Bp.QUALITY_LOW)
			_step = 3
			_step_frame(5)
		3:
			if not _waited():
				return false
			var sun := _world.get_node(^"Sun") as DirectionalLight3D
			var p: Dictionary = root.get_node(^"Quality").profile()
			_check(int(p["tier"]) == Bp.QUALITY_LOW, "tier switched to low")
			_check(sun.directional_shadow_mode == DirectionalLight3D.SHADOW_PARALLEL_2_SPLITS, "low tier: 2 splits on the sun")
			_check(is_equal_approx(sun.directional_shadow_max_distance, float(p["shadow_distance_m"])), "low tier: shadow distance")
			_check(_world.get_viewport().positional_shadow_atlas_size == 0, "low tier: no shadowed positional lights")
			root.get_node(^"Settings").set_value(&"graphics.tier", _tier_before)
			_world.queue_free()
			_warmup = (load("res://world/warmup.tscn") as PackedScene).instantiate() as Node3D
			(_warmup.get_node(^"World/View") as BpWorldView).warmed_up.connect(func() -> void: _warmed = true)
			root.add_child(_warmup)
			_step = 4
			_step_frame(120)
		4:
			if not _warmed and not _waited():
				return false
			_check(_warmed, "warm-up scene reports warmed_up")
			_check(_warmup.get_node_or_null(^"Samples") != null and _warmup.get_node(^"Samples").get_child_count() == 8, "warm-up draws a sample of each of the 8 shaders")
			_warmup.queue_free()
			_dock = (load("res://world/dock_stage.tscn") as PackedScene).instantiate() as Node3D
			root.add_child(_dock)
			_step = 5
			_step_frame(3)
		5:
			if not _waited():
				return false
			_dock_stage()
			return _finish()
	return false


var _until := 0


func _step_frame(n: int) -> void:
	_until = _frame + n


func _waited() -> bool:
	return _frame >= _until


func _presets_and_lights() -> void:
	var presets: Node = _world.get_node(^"Presets")
	var env := (_world.get_node(^"Environment") as WorldEnvironment).environment
	var sun := _world.get_node(^"Sun") as DirectionalLight3D
	var moon := _world.get_node(^"Moon") as DirectionalLight3D
	_check(sun.get_index() < moon.get_index(), "the sun comes before the moon (sky LIGHT0, LIGHT1)")
	_check(env.fog_height_density == 0.0 and env.fog_sky_affect == 0.0, "no height fog, no depth fog over the sky")
	_check(env.glow_blend_mode == Environment.GLOW_BLEND_MODE_SCREEN and env.glow_hdr_threshold >= 1.0 and env.glow_bloom == 0.0, "glow screen, threshold 1, bloom 0")
	_check(env.tonemap_mode == Environment.TONE_MAPPER_AGX and is_equal_approx(env.tonemap_agx_white, 8.0) and is_equal_approx(env.tonemap_agx_contrast, 1.25), "AgX white 8, contrast 1.25")
	_check(env.background_mode == Environment.BG_SKY and not env.ssil_enabled and not env.sdfgi_enabled, "sky background, no SSIL or SDFGI")
	_check((_world.get_node(^"Camera") as Camera3D).far >= 20000.0, "camera far at least 20 km")

	# Dusk clear, then halfway from clear to overcast.
	var sun_dir: Vector3 = presets.call(&"sky_dir", 250.0, 9.0)
	presets.call(&"apply", Bp.TIME_DUSK, Bp.WEATHER_CLEAR, Bp.WEATHER_CLEAR, 1.0, sun_dir, 1.3e-4, 0.2, 0.7, Vector2.ZERO, 90.0, 7, 0.0)
	var clear_sun := sun.light_energy
	var clear_fog := env.fog_light_color
	var clear_exposure := env.tonemap_exposure
	_check(sun.sky_mode == DirectionalLight3D.SKY_MODE_LIGHT_AND_SKY and sun.shadow_enabled and not moon.visible, "dusk: sun lights and casts, no moon")
	_check((-sun.global_transform.basis.z).dot(-sun_dir) > 0.999, "sun shines away from the sun direction")
	presets.call(&"apply", Bp.TIME_DUSK, Bp.WEATHER_OVERCAST, Bp.WEATHER_OVERCAST, 1.0, sun_dir, 3.3e-4, 0.2, 0.5, Vector2.ZERO, 90.0, 7, 0.0)
	var over_sun := sun.light_energy
	var over_fog := env.fog_light_color
	presets.call(&"apply", Bp.TIME_DUSK, Bp.WEATHER_CLEAR, Bp.WEATHER_OVERCAST, 0.5, sun_dir, 2.0e-4, 0.2, 0.6, Vector2.ZERO, 90.0, 7, 0.0)
	_check(is_equal_approx(sun.light_energy, (clear_sun + over_sun) * 0.5), "sun energy blends halfway (%f)" % sun.light_energy)
	_check(env.fog_light_color.r < maxf(clear_fog.r, over_fog.r) + 1e-4 and env.fog_light_color.r > minf(clear_fog.r, over_fog.r) - 1e-4, "fog colour between the two presets")
	_check(is_equal_approx(env.fog_density, 2.0e-4), "fog density is the simulation's")
	_check(env.tonemap_exposure >= clear_exposure, "overcast exposes up")

	# Night: the sun stays, dark and in the sky only; the moon lights and casts; the ambient floor holds.
	presets.call(&"apply", Bp.TIME_NIGHT, Bp.WEATHER_CLEAR, Bp.WEATHER_CLEAR, 1.0, presets.call(&"sky_dir", 250.0, -20.0), 1.3e-4, 0.0, 0.7, Vector2(120.0, 40.0), 90.0, 7, 0.0)
	_check(sun.visible and sun.light_energy == 0.0 and sun.sky_mode == DirectionalLight3D.SKY_MODE_SKY_ONLY and not sun.shadow_enabled, "night: sun dark, sky only")
	_check(moon.visible and moon.light_energy > 0.0 and moon.shadow_enabled, "night: the moon lights and casts")
	_check((-moon.global_transform.basis.z).y < -0.3, "night: the moon is up")
	var c := env.ambient_light_color.srgb_to_linear()
	var lum := 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
	_check(lum * (1.0 - env.ambient_light_sky_contribution) >= 0.035 - 1e-3, "night: ambient floor kept (%f)" % (lum * (1.0 - env.ambient_light_sky_contribution)))
	var sky := env.sky.sky_material as ShaderMaterial
	_check(float(sky.get_shader_parameter(&"star_energy")) > 0.0 and float(sky.get_shader_parameter(&"sun_glow")) == 0.0, "night: stars, no sun glow")

	# Bad input never throws or leaves NaN.
	presets.call(&"apply", 99, -4, 77, NAN, Vector3(NAN, 0.0, 0.0), NAN, INF, NAN, Vector2(INF, 0.0), NAN, 0, NAN)
	_check(is_finite(env.fog_density) and is_finite(sun.light_energy) and is_finite(env.tonemap_exposure), "bad input clamps to finite values")


func _quality_profile() -> void:
	var p: Dictionary = root.get_node(^"Quality").profile()
	var vp := _world.get_viewport()
	var env := (_world.get_node(^"Environment") as WorldEnvironment).environment
	var msaa := {0: Viewport.MSAA_DISABLED, 2: Viewport.MSAA_2X, 4: Viewport.MSAA_4X}
	_check(vp.msaa_3d == msaa.get(int(p["msaa"]), -1), "viewport MSAA follows the profile")
	_check(is_equal_approx(vp.scaling_3d_scale, float(p["scale_3d"])), "3D scale follows the profile")
	_check(env.glow_enabled == (int(p["glow_max"]) > 0), "glow on as the profile says")
	_check(env.ssao_enabled == (StringName(p["ssao"]) != &"off"), "SSAO as the profile says")
	_check(not vp.use_taa, "no temporal AA")
	var sun := _world.get_node(^"Sun") as DirectionalLight3D
	_check(sun.shadow_caster_mask & (1 << 3) == 0, "VFX layer casts no shadow")
	_check(is_equal_approx(sun.shadow_blur, 1.0), "shadow blur 1")


func _dock_stage() -> void:
	var water := _dock.get_node(^"Water") as MeshInstance3D
	var env := (_dock.get_node(^"Environment") as WorldEnvironment)
	_check(water.material_override is ShaderMaterial, "dock water uses the water shader")
	if FileAccess.file_exists("res://art/gen/ocean_detail_normals.bin"):
		var m := water.material_override as ShaderMaterial
		_check(m.get_shader_parameter(&"detail_normals") is Texture2DArray, "dock water binds the baked detail")
		_check(is_equal_approx(float(m.get_shader_parameter(&"detail_strength")), 0.25), "dock detail at quarter strength")
	_check(water.cast_shadow == GeometryInstance3D.SHADOW_CASTING_SETTING_OFF, "dock water casts no shadow")
	_check(env.environment != null and (_dock.get_node(^"Key") as Light3D).visible, "dock environment on when idle")
	_dock.call(&"_on_session_state", Bp.SESSION_IN_SORTIE)
	_check(env.environment == null and not (_dock.get_node(^"Key") as Light3D).visible and not water.visible, "dock steps aside in a sortie")
	_dock.call(&"_on_session_state", Bp.SESSION_IDLE)
	_check(env.environment != null and water.visible, "dock returns when the sortie ends")
	var cam := (_dock.get_node(^"Camera") as Camera3D).attributes as CameraAttributesPractical
	_check(not cam.auto_exposure_enabled, "dock: no auto exposure")


func _finish() -> bool:
	if _failures == 0:
		print("world_look_test: all passed")
	quit(1 if _failures > 0 else 0)
	return true
