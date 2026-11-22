extends Node3D
## Capture harness for the sea, sky, cloud and island shaders (units shader-water, shader-sky).
## DESIGN.md L1826-1917. It builds a scene with no simulation behind it: the clipmap layout of
## hull_mesh::env::ocean, real wave sets from sim-core (wave_fixture.json), stand-in clouds, an island
## and block hulls, lit by the spec's environment numbers. It then saves one PNG per shot.
##
## Windowed only (the headless renderer compiles no shader):
##   godot --path game --resolution 1600x900 res://test/capture/sea_capture.tscn -- --out=/abs/dir
## Options after "--": --out=DIR (default user://captures), --shots=a,b (default all), --tier=low|medium|high
## (clipmap, default low), --bench (print the frame-time cost of the sea instead of capturing; with
## --scale=S for the 3D resolution scale and --shot=N for the index of the shot to time).
## Needs the generated files of `cargo xtask gen` (wave include; detail and noise textures are optional
## and the shot is drawn without them when missing). Headless, and under check_project, it does nothing.

const TOKENS_PATH: String = "res://art/tokens/world_tokens.json"
const FIXTURE_PATH: String = "res://test/capture/wave_fixture.json"
const WAVE_GLOBALS: Array[StringName] = [&"bp_waves_0", &"bp_waves_1", &"bp_waves_2", &"bp_waves_3", &"bp_waves_4", &"bp_waves_5", &"bp_waves_6", &"bp_waves_7"]
const DETAIL_LOOP_S: float = 24.0
const SETTLE_FRAMES: int = 24
## Clipmap table (DESIGN.md L1897-1901): levels, n, s0.
const TIERS: Dictionary = {"low": [8, 63, 1.0], "medium": [10, 63, 0.5], "high": [9, 127, 0.5]}
## Weather presets: the spec's fog table (L1859-1866) and this harness's light levels.
const PRESETS: Dictionary = {
	"dusk_clear": {"time": "dusk", "fog": "clear", "sun_elevation": 12.0, "sun_energy": 3.0, "fog_density": 1.3e-4, "aerial": 0.7, "overcast": 0.0, "cirrus": 0.4, "cloud_cover": 0.3, "stars": 0.0, "rain": 0.0, "waves": "clear_4"},
	"dusk_clear_3": {"time": "dusk", "fog": "clear", "sun_elevation": 12.0, "sun_energy": 3.0, "fog_density": 1.3e-4, "aerial": 0.7, "overcast": 0.0, "cirrus": 0.4, "cloud_cover": 0.3, "stars": 0.0, "rain": 0.0, "waves": "clear_3"},
	"dusk_calm": {"time": "dusk", "fog": "clear", "sun_elevation": 6.0, "sun_energy": 2.6, "fog_density": 1.3e-4, "aerial": 0.7, "overcast": 0.0, "cirrus": 0.35, "cloud_cover": 0.15, "stars": 0.0, "rain": 0.0, "waves": "calm"},
	"dusk_overcast": {"time": "dusk", "fog": "overcast", "sun_elevation": 12.0, "sun_energy": 0.8, "fog_density": 3.3e-4, "aerial": 0.5, "overcast": 1.0, "cirrus": 0.0, "cloud_cover": 1.0, "stars": 0.0, "rain": 0.0, "waves": "overcast_5"},
	"dusk_storm": {"time": "dusk", "fog": "storm", "sun_elevation": 12.0, "sun_energy": 0.35, "fog_density": 2.6e-3, "aerial": 0.2, "overcast": 1.0, "cirrus": 0.0, "cloud_cover": 1.0, "stars": 0.0, "rain": 0.8, "waves": "storm_7"},
	"day_clear": {"time": "day", "fog": "clear", "sun_elevation": 42.0, "sun_energy": 3.2, "fog_density": 1.3e-4, "aerial": 0.7, "overcast": 0.0, "cirrus": 0.4, "cloud_cover": 0.3, "stars": 0.0, "rain": 0.0, "waves": "clear_3"},
	"night_clear": {"time": "night", "fog": "clear", "sun_elevation": -30.0, "sun_energy": 0.0, "fog_density": 1.3e-4, "aerial": 0.7, "overcast": 0.0, "cirrus": 0.3, "cloud_cover": 0.3, "stars": 1.0, "rain": 0.0, "waves": "clear_3"},
}
## Shots: preset, camera position (render space), bearing off the sun (degrees, 0 looks at the sun), pitch.
const SHOTS: Array[Dictionary] = [
	{"name": "dusk_clear_into_sun", "preset": "dusk_clear", "eye": Vector3(0.0, 15.0, 0.0), "off_sun": 8.0, "pitch": -7.0},
	{"name": "dusk_clear_cross", "preset": "dusk_clear", "eye": Vector3(0.0, 9.0, 0.0), "off_sun": 82.0, "pitch": -6.0},
	{"name": "dusk_clear_down_sun", "preset": "dusk_clear", "eye": Vector3(0.0, 22.0, 0.0), "off_sun": 168.0, "pitch": -12.0},
	{"name": "dusk_clear_near", "preset": "dusk_clear", "eye": Vector3(0.0, 3.2, 0.0), "off_sun": 48.0, "pitch": -14.0},
	{"name": "dusk_clear_high", "preset": "dusk_clear", "eye": Vector3(0.0, 300.0, 0.0), "off_sun": 30.0, "pitch": -22.0},
	{"name": "dusk_clear_no_glow", "preset": "dusk_clear", "eye": Vector3(0.0, 15.0, 0.0), "off_sun": 30.0, "pitch": -7.0, "plain": true},
	{"name": "dusk_state_3", "preset": "dusk_clear_3", "eye": Vector3(0.0, 9.0, 0.0), "off_sun": 60.0, "pitch": -6.0},
	{"name": "dusk_calm", "preset": "dusk_calm", "eye": Vector3(0.0, 9.0, 0.0), "off_sun": 20.0, "pitch": -5.0},
	{"name": "dusk_overcast", "preset": "dusk_overcast", "eye": Vector3(0.0, 12.0, 0.0), "off_sun": 40.0, "pitch": -7.0},
	{"name": "dusk_storm", "preset": "dusk_storm", "eye": Vector3(0.0, 14.0, 0.0), "off_sun": 60.0, "pitch": -8.0},
	{"name": "overcast_high", "preset": "dusk_overcast", "eye": Vector3(0.0, 120.0, 0.0), "off_sun": 110.0, "pitch": -24.0, "frozen": true},
	{"name": "storm_high", "preset": "dusk_storm", "eye": Vector3(0.0, 120.0, 0.0), "off_sun": 110.0, "pitch": -24.0, "frozen": true},
	{"name": "storm_deck", "preset": "dusk_storm", "eye": Vector3(0.0, 14.0, 0.0), "off_sun": 60.0, "pitch": -8.0, "frozen": true},
	{"name": "day_clear", "preset": "day_clear", "eye": Vector3(0.0, 12.0, 0.0), "off_sun": 70.0, "pitch": -7.0},
	{"name": "night_clear", "preset": "night_clear", "eye": Vector3(0.0, 12.0, 0.0), "off_sun": 195.0, "pitch": 14.0},
	{"name": "cirrus_along_sun", "preset": "dusk_clear", "eye": Vector3(0.0, 15.0, 0.0), "off_sun": 8.0, "pitch": 4.0, "cirrus_off_sun": 0.0},
	{"name": "cirrus_across_sun", "preset": "dusk_clear", "eye": Vector3(0.0, 15.0, 0.0), "off_sun": 8.0, "pitch": 4.0, "cirrus_off_sun": 90.0},
	{"name": "into_sun_no_fog", "preset": "dusk_clear", "eye": Vector3(0.0, 15.0, 0.0), "off_sun": 8.0, "pitch": -7.0, "no_fog": true},
	{"name": "cross_screen_space", "preset": "dusk_clear", "eye": Vector3(0.0, 9.0, 0.0), "off_sun": 82.0, "pitch": -6.0, "screen_space": true},
	{"name": "shore", "preset": "dusk_clear_3", "eye": Vector3(-60.0, 14.0, 60.0), "look_at": Vector3(-200.0, 0.0, 150.0)},
	{"name": "hull_mask", "preset": "dusk_clear_3", "eye": Vector3(-40.0, 45.0, 20.0), "look_at": Vector3(-65.0, 0.0, -10.0), "hide_hulls": true},
	{"name": "markers_near", "preset": "dusk_clear", "eye": Vector3(1.0, 2.6, 5.0), "look_at": Vector3(1.0, 0.0, -14.0), "frozen": true, "markers": true},
	{"name": "markers_far", "preset": "dusk_clear", "eye": Vector3(0.0, 7.0, 0.0), "look_at": Vector3(-15.0, 0.0, -75.0), "fov": 16.0, "frozen": true, "markers": true},
	{"name": "markers_storm", "preset": "dusk_storm", "eye": Vector3(1.0, 9.0, 8.0), "look_at": Vector3(1.0, 0.0, -14.0), "frozen": true, "markers": true},
]

var _tokens: Dictionary = {}
var _fixture: Dictionary = {}
var _origin: Vector3 = Vector3.ZERO
var _t0: float = 0.0
var _env: Environment
var _sky_material: ShaderMaterial
var _water: ShaderMaterial
var _cloud_material: ShaderMaterial
var _island_material: ShaderMaterial
var _sun: DirectionalLight3D
var _moon: DirectionalLight3D
var _camera: Camera3D
var _sea: Node3D
var _markers: Node3D
var _scenery: Node3D
var _spec: Array = []
var _sun_azimuth_deg: float = 250.0
var _kept: Array[Resource] = []
var _has_detail: bool = false
var _has_cloud_noise: bool = false
var _hulls: Array[Node3D] = []
## Islands as [centre, radius, peak, noise], for the shore heightfield.
var _islands: Array[Array] = []


func _ready() -> void:
	if DisplayServer.get_name() == "headless":
		return
	_run.call_deferred()


func _run() -> void:
	var options: Dictionary = _options()
	_tokens = _read_json(TOKENS_PATH)
	_fixture = _read_json(FIXTURE_PATH)
	if _tokens.is_empty() or _fixture.is_empty():
		push_error("sea_capture: tokens or wave fixture missing")
		get_tree().quit(1)
		return
	var origin: Array = _fixture["origin"]
	_origin = Vector3(origin[0], origin[1], origin[2])
	_t0 = _fixture["t0"]
	_spec = TIERS.get(options.get("tier", "low"), TIERS["low"])
	_register_globals()
	_build_environment()
	_build_sea()
	_build_scenery()
	_build_shore()
	_build_markers()
	print("sea_capture: driver ", RenderingServer.get_current_rendering_driver_name(), ", method ", RenderingServer.get_current_rendering_method(), ", adapter ", RenderingServer.get_video_adapter_name())
	if options.has("bench"):
		await _bench(float(options.get("scale", "1.0")), int(options.get("shot", "1")))
		get_tree().quit()
		return
	var out_dir: String = options.get("out", "user://captures")
	DirAccess.make_dir_recursive_absolute(out_dir)
	var wanted: PackedStringArray = PackedStringArray()
	if options.has("shots"):
		wanted = String(options["shots"]).split(",")
	for shot: Dictionary in SHOTS:
		if not wanted.is_empty() and not wanted.has(shot["name"]):
			continue
		_apply_shot(shot)
		await _draw_frames(SETTLE_FRAMES)
		var image: Image = get_viewport().get_texture().get_image()
		var path: String = out_dir.path_join(String(shot["name"]) + ".png")
		var err: Error = image.save_png(path)
		print("sea_capture: ", path, " ", "ok" if err == OK else "FAILED")
	get_tree().quit()


## Draw `count` frames now. force_draw keeps the capture going when the window is hidden or the display
## sleeps, where the engine's own loop stops drawing.
func _draw_frames(count: int) -> void:
	for i: int in count:
		await get_tree().process_frame
		RenderingServer.force_draw(true, 1.0 / 60.0)


func _options() -> Dictionary:
	var out: Dictionary = {}
	for arg: String in OS.get_cmdline_user_args():
		var body: String = arg.trim_prefix("--")
		var eq: int = body.find("=")
		if eq < 0:
			out[body] = true
		else:
			out[body.substr(0, eq)] = body.substr(eq + 1)
	return out


func _read_json(path: String) -> Dictionary:
	if not FileAccess.file_exists(path):
		return {}
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(path))
	return parsed if parsed is Dictionary else {}


## A token entry's colour as the Color a source_color uniform or a light expects (sRGB-encoded).
func _color(entry: Dictionary) -> Color:
	var lin: Array = entry["linear_rgb"]
	return Color(lin[0], lin[1], lin[2]).linear_to_srgb()


func _token(group: String, key: String, value: String) -> Dictionary:
	for entry: Dictionary in _tokens[group]:
		if entry.get(key, "") == value:
			return entry
	push_error("sea_capture: no token " + group + " " + value)
	return {"linear_rgb": [0.0, 0.0, 0.0]}


func _fog_token(weather: String, time: String) -> Dictionary:
	for entry: Dictionary in _tokens["fog"]:
		if entry["weather"] == weather and entry["time"] == time:
			return entry
	return {"linear_rgb": [0.0, 0.0, 0.0]}


# --- generated textures (hull_mesh::gen_files::TexBlob) ---

## Layers of a TexBlob file as images, or an empty array when the file is missing or malformed.
func _load_blob(path: String) -> Array[Image]:
	var images: Array[Image] = []
	if not FileAccess.file_exists(path):
		return images
	var bytes: PackedByteArray = FileAccess.get_file_as_bytes(path)
	if bytes.size() < 32 or bytes.slice(0, 4).get_string_from_ascii() != "BPTX" or bytes.decode_u32(4) != 1:
		return images
	var format: int = bytes.decode_u32(12)
	var width: int = bytes.decode_u32(16)
	var height: int = bytes.decode_u32(20)
	var layers: int = bytes.decode_u32(24)
	var mips: int = bytes.decode_u32(28)
	if layers <= 0 or (bytes.size() - 32) % layers != 0:
		return images
	var per_layer: int = (bytes.size() - 32) / layers
	for i: int in layers:
		var data: PackedByteArray = bytes.slice(32 + i * per_layer, 32 + (i + 1) * per_layer)
		images.append(Image.create_from_data(width, height, mips > 1, format as Image.Format, data))
	return images


func _has_global(global: StringName) -> bool:
	return ProjectSettings.has_setting("shader_globals/" + global)


func _register_globals() -> void:
	# Globals the project does not register yet are added here, before any shader that names them loads.
	var zero: Projection = Projection(Vector4.ZERO, Vector4.ZERO, Vector4.ZERO, Vector4.ZERO)
	for global: StringName in WAVE_GLOBALS:
		if not _has_global(global):
			RenderingServer.global_shader_parameter_add(global, RenderingServer.GLOBAL_VAR_TYPE_MAT4, zero)
	if not _has_global(&"bp_waves_t_ref"):
		RenderingServer.global_shader_parameter_add(&"bp_waves_t_ref", RenderingServer.GLOBAL_VAR_TYPE_FLOAT, 0.0)
	if not _has_global(&"bp_cloud_shadow"):
		RenderingServer.global_shader_parameter_add(&"bp_cloud_shadow", RenderingServer.GLOBAL_VAR_TYPE_SAMPLER2D, RID())
	if not _has_global(&"bp_cloud_shadow_xf"):
		RenderingServer.global_shader_parameter_add(&"bp_cloud_shadow_xf", RenderingServer.GLOBAL_VAR_TYPE_VEC4, Vector4.ZERO)
	RenderingServer.global_shader_parameter_set(&"bp_waves_t_ref", 0.0)
	var noise: Array[Image] = _load_blob("res://art/gen/noise_cloud_shadow.bin")
	if noise.is_empty():
		print("sea_capture: art/gen/noise_cloud_shadow.bin missing (cargo xtask gen); no cloud shadows")
	else:
		var texture: ImageTexture = ImageTexture.create_from_image(noise[0])
		_kept.append(texture)
		RenderingServer.global_shader_parameter_set(&"bp_cloud_shadow", texture.get_rid())
		_has_cloud_noise = true


## Wave uniforms of a fixture set: 8 column-major mat4s. Frozen zeroes every omega, so the surface is the
## fixture's t0 whatever TIME says, and the fixture's probe heights apply.
func _apply_waves(set_name: String, frozen: bool) -> void:
	var wave_set: Dictionary = _fixture["sets"][set_name]
	var mats: Array = wave_set["mats"]
	for i: int in 8:
		var m: Array = mats[i]
		var cols: Array[Vector4] = []
		for c: int in 4:
			cols.append(Vector4(m[c * 4], m[c * 4 + 1], m[c * 4 + 2], m[c * 4 + 3]))
		if frozen:
			cols[0].w = 0.0
			cols[2].w = 0.0
		RenderingServer.global_shader_parameter_set(WAVE_GLOBALS[i], Projection(cols[0], cols[1], cols[2], cols[3]))
	_water.set_shader_parameter(&"sea_state", float(wave_set["sea_state"]))


# --- environment ---

func _build_environment() -> void:
	_sky_material = ShaderMaterial.new()
	_sky_material.shader = load("res://shaders/sky.gdshader")
	var sky: Sky = Sky.new()
	sky.sky_material = _sky_material
	sky.radiance_size = Sky.RADIANCE_SIZE_256
	sky.process_mode = Sky.PROCESS_MODE_INCREMENTAL
	_env = Environment.new()
	_env.background_mode = Environment.BG_SKY
	_env.sky = sky
	_env.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
	_env.reflected_light_source = Environment.REFLECTION_SOURCE_SKY
	_env.tonemap_mode = Environment.TONE_MAPPER_AGX
	_env.tonemap_agx_white = 8.0
	_env.tonemap_agx_contrast = 1.25
	_env.glow_enabled = true
	_env.glow_blend_mode = Environment.GLOW_BLEND_MODE_SCREEN
	_env.glow_hdr_threshold = 1.0
	_env.glow_bloom = 0.0
	_env.set(&"glow_levels/1", 0.0)
	_env.set(&"glow_levels/2", 1.0)
	_env.set(&"glow_levels/3", 1.0)
	_env.set(&"glow_levels/4", 0.0)
	_env.set(&"glow_levels/5", 0.0)
	_env.fog_enabled = true
	_env.fog_height_density = 0.0
	# The sky shader draws its own haze; depth fog must not paint over it.
	_env.fog_sky_affect = 0.0
	var world: WorldEnvironment = WorldEnvironment.new()
	world.environment = _env
	add_child(world)
	# The sun is created first so it is LIGHT0 in the sky shader; the moon is LIGHT1.
	_sun = DirectionalLight3D.new()
	_sun.shadow_enabled = true
	_sun.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_2_SPLITS
	_sun.directional_shadow_max_distance = 250.0
	_sun.light_angular_distance = 0.5
	add_child(_sun)
	_moon = DirectionalLight3D.new()
	_moon.light_angular_distance = 0.5
	add_child(_moon)
	_camera = Camera3D.new()
	_camera.far = 20000.0
	_camera.near = 0.3
	_camera.fov = 60.0
	_camera.keep_aspect = Camera3D.KEEP_HEIGHT
	add_child(_camera)
	_camera.make_current()


## Unit vector towards a body at a compass azimuth (0 = north = -Z, clockwise) and elevation.
func _sky_dir(azimuth_deg: float, elevation_deg: float) -> Vector3:
	var az: float = deg_to_rad(azimuth_deg)
	var el: float = deg_to_rad(elevation_deg)
	return Vector3(sin(az) * cos(el), sin(el), -cos(az) * cos(el))


func _apply_preset(preset_name: String, plain: bool) -> void:
	var p: Dictionary = PRESETS[preset_name]
	var time: String = p["time"]
	var sky_tokens: Dictionary = _token("sky", "time", time)
	var fog: Color = _color(_fog_token(p["fog"], time))
	var key: Color = _color(sky_tokens["key_light"])
	var night: bool = time == "night"
	var deep: Color = _color(_token("sea", "token", "sea.deep"))
	_sky_material.set_shader_parameter(&"zenith_color", _color(sky_tokens["zenith"]))
	_sky_material.set_shader_parameter(&"horizon_color", _color(sky_tokens["horizon"]))
	_sky_material.set_shader_parameter(&"ground_color", deep.lerp(_color(sky_tokens["horizon"]), 0.35))
	_sky_material.set_shader_parameter(&"overcast_color", fog)
	_sky_material.set_shader_parameter(&"cirrus_color", _color(sky_tokens["horizon"]).lerp(key, 0.6) if not night else _color(sky_tokens["horizon"]) * 2.0)
	_sky_material.set_shader_parameter(&"overcast", p["overcast"])
	_sky_material.set_shader_parameter(&"cirrus_cover", p["cirrus"])
	_sky_material.set_shader_parameter(&"cirrus_dir", Vector2(sin(1.1), -cos(1.1)))
	_sky_material.set_shader_parameter(&"cirrus_offset", Vector2(0.37, 1.9))
	_sky_material.set_shader_parameter(&"star_energy", p["stars"])
	_sky_material.set_shader_parameter(&"rain", p["rain"])
	_sky_material.set_shader_parameter(&"sun_glow", 0.0 if night else (1.0 - float(p["overcast"])) * (0.55 if time == "dusk" else 0.25))
	_sky_material.set_shader_parameter(&"moon_phase", 0.38)

	var sun_dir: Vector3 = _sky_dir(_sun_azimuth_deg, p["sun_elevation"])
	_sun.look_at_from_position(Vector3.ZERO, -sun_dir, Vector3.UP)
	_sun.light_color = key if not night else Color.WHITE
	_sun.light_energy = p["sun_energy"]
	_sun.shadow_enabled = not night
	# At night the sun stays in the tree with no energy, so the moon keeps its place as LIGHT1.
	_sun.sky_mode = DirectionalLight3D.SKY_MODE_LIGHT_AND_SKY if not night else DirectionalLight3D.SKY_MODE_SKY_ONLY
	_moon.visible = night
	_moon.look_at_from_position(Vector3.ZERO, -_sky_dir(_sun_azimuth_deg + 185.0, 34.0), Vector3.UP)
	_moon.light_color = key
	_moon.light_energy = 0.35
	_moon.shadow_enabled = night

	_env.fog_light_color = fog
	_env.fog_density = p["fog_density"]
	_env.fog_aerial_perspective = p["aerial"]
	_env.fog_sun_scatter = 0.2 * (1.0 - smoothstep(10.0, 25.0, float(p["sun_elevation"]))) if not night else 0.0
	# Rule A0 (L1783): the plain shot has no glow (SSAO, SSR and volumetric fog are never on here).
	_env.glow_enabled = not plain

	_water.set_shader_parameter(&"rain", p["rain"])
	var cover: float = p["cloud_cover"]
	var uv_per_m: float = 1.0 / 6000.0
	RenderingServer.global_shader_parameter_set(&"bp_cloud_shadow_xf", Vector4(fposmod(_origin.x * uv_per_m, 1.0), fposmod(_origin.z * uv_per_m, 1.0), uv_per_m, cover if _has_cloud_noise else 0.0))
	_island_material.set_shader_parameter(&"wet_height_m", maxf(0.5, 0.5 * float(_fixture["sets"][p["waves"]]["hs"])))


func _apply_shot(shot: Dictionary) -> void:
	var preset: String = shot["preset"]
	var p: Dictionary = PRESETS[preset]
	_apply_preset(preset, shot.get("plain", false))
	if shot.has("cirrus_off_sun"):
		var wind: float = deg_to_rad(_sun_azimuth_deg + float(shot["cirrus_off_sun"]))
		_sky_material.set_shader_parameter(&"cirrus_dir", Vector2(sin(wind), -cos(wind)))
	_env.fog_enabled = not shot.get("no_fog", false)
	# SSR and SSAO as the High tier sets them (L1838-1839), to check the opaque water takes both.
	var screen_space: bool = shot.get("screen_space", false)
	_env.ssr_enabled = screen_space
	_env.ssr_max_steps = 32
	_env.ssao_enabled = screen_space
	_env.ssao_radius = 1.0
	_apply_waves(p["waves"], shot.get("frozen", false))
	_markers.visible = shot.get("markers", false)
	for hull: Node3D in _hulls:
		hull.visible = not shot.get("hide_hulls", false)
	for child: Node in _markers.get_children():
		child.visible = child.name.begins_with(String(p["waves"]) + "_")
	var eye: Vector3 = shot["eye"]
	_camera.fov = shot.get("fov", 60.0)
	if shot.has("look_at"):
		_camera.look_at_from_position(eye, shot["look_at"], Vector3.UP)
	else:
		var bearing: float = deg_to_rad(_sun_azimuth_deg + float(shot["off_sun"]))
		var pitch: float = deg_to_rad(float(shot["pitch"]))
		var forward: Vector3 = Vector3(sin(bearing) * cos(pitch), sin(pitch), -cos(bearing) * cos(pitch))
		_camera.look_at_from_position(eye, eye + forward, Vector3.UP)
	_place_sea(eye)


# --- sea: the clipmap of hull_mesh::env::ocean, rebuilt here from its documented layout ---

func _build_sea() -> void:
	_water = ShaderMaterial.new()
	_water.shader = load("res://shaders/water.gdshader")
	for token: String in ["deep", "scatter", "shallow", "foam"]:
		_water.set_shader_parameter(StringName(token + "_color"), _color(_token("sea", "token", "sea." + token)))
	var normals: Array[Image] = _load_blob("res://art/gen/ocean_detail_normals.bin")
	var turbulence: Array[Image] = _load_blob("res://art/gen/ocean_detail_turbulence.bin")
	if normals.is_empty() or turbulence.is_empty():
		print("sea_capture: art/gen/ocean_detail_*.bin missing (cargo xtask gen); drawing without detail")
	else:
		var normal_array: Texture2DArray = Texture2DArray.new()
		normal_array.create_from_images(normals)
		var turbulence_array: Texture2DArray = Texture2DArray.new()
		turbulence_array.create_from_images(turbulence)
		_water.set_shader_parameter(&"detail_normals", normal_array)
		_water.set_shader_parameter(&"detail_turbulence", turbulence_array)
		_water.set_shader_parameter(&"detail_strength", 1.0)
		_has_detail = true
	_water.set_shader_parameter(&"detail_phase", fposmod(_t0 / DETAIL_LOOP_S, 1.0))
	_water.set_shader_parameter(&"detail_origin", Vector4(fposmod(_origin.x / 41.0, 1.0), fposmod(_origin.z / 41.0, 1.0), fposmod(_origin.x / 11.3, 1.0), fposmod(_origin.z / 11.3, 1.0)))

	_sea = Node3D.new()
	add_child(_sea)
	var levels: int = _spec[0]
	var vertices: int = 0
	var triangles: int = 0
	for level: int in levels:
		var mesh: ArrayMesh = _level_mesh(level)
		vertices += mesh.surface_get_array_len(0)
		triangles += mesh.surface_get_array_index_len(0) / 3
		_add_sea_instance(mesh)
	_add_sea_instance(_skirt_mesh())
	print("sea_capture: clipmap ", levels, " levels, ", vertices, " vertices, ", triangles, " triangles")


func _add_sea_instance(mesh: ArrayMesh) -> void:
	var instance: MeshInstance3D = MeshInstance3D.new()
	instance.mesh = mesh
	instance.material_override = _water
	instance.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	instance.extra_cull_margin = 64.0
	_sea.add_child(instance)


func _grid() -> Dictionary:
	var n: int = _spec[1]
	var q: int = ((maxi(n - 1, 14) - 2) / 4) * 4 + 2
	var hi: int = q / 2 + 1
	var band: float = minf(maxf(n / 10.0, 3.0), hi / 2.0 - 1.0)
	return {"q": q, "lo": -(q / 2 - 1), "hi": hi, "band": band, "hole_lo": -(hi - 4) / 2, "hole_hi": hi / 2}


func _spacing(level: int) -> float:
	return float(_spec[2]) * float(1 << level)


func _level_mesh(level: int) -> ArrayMesh:
	var g: Dictionary = _grid()
	var lo: int = g["lo"]
	var hi: int = g["hi"]
	var h0: int = g["hole_lo"]
	var h1: int = g["hole_hi"]
	var band: float = g["band"]
	var s: float = _spacing(level)
	var ring: bool = level > 0
	var side: int = hi - lo + 1
	var index: PackedInt32Array = PackedInt32Array()
	index.resize(side * side)
	index.fill(-1)
	var positions: PackedVector3Array = PackedVector3Array()
	var normals: PackedVector3Array = PackedVector3Array()
	var custom: PackedFloat32Array = PackedFloat32Array()
	for j: int in range(lo, hi + 1):
		for i: int in range(lo, hi + 1):
			if ring and i > h0 and i < h1 and j > h0 and j < h1:
				continue
			index[(i - lo) + side * (j - lo)] = positions.size()
			positions.append(Vector3(i * s, 0.0, j * s))
			normals.append(Vector3.UP)
			var d: float = float(mini(mini(i - lo, hi - i), mini(j - lo, hi - j)))
			var morph: float = 1.0 if d <= 2.0 else clampf(1.0 - (d - 2.0) / (band - 2.0), 0.0, 1.0)
			custom.append_array(PackedFloat32Array([s, morph, float(level), 0.0]))
	var indices: PackedInt32Array = PackedInt32Array()
	for j: int in range(lo, hi):
		for i: int in range(lo, hi):
			if ring and i >= h0 and i < h1 and j >= h0 and j < h1:
				continue
			var v00: int = index[(i - lo) + side * (j - lo)]
			var v10: int = index[(i + 1 - lo) + side * (j - lo)]
			var v01: int = index[(i - lo) + side * (j + 1 - lo)]
			var v11: int = index[(i + 1 - lo) + side * (j + 1 - lo)]
			indices.append_array(PackedInt32Array([v00, v10, v01, v10, v11, v01]))
	return _sea_mesh(positions, normals, custom, indices)


func _skirt_mesh() -> ArrayMesh:
	var g: Dictionary = _grid()
	var lo: int = g["lo"]
	var hi: int = g["hi"]
	var level: int = int(_spec[0]) - 1
	var s: float = _spacing(level)
	var inner: Array[Vector2] = []
	for i: int in range(lo, hi, 2):
		inner.append(Vector2(i, lo))
	for j: int in range(lo, hi, 2):
		inner.append(Vector2(hi, j))
	for i: int in range(hi, lo, -2):
		inner.append(Vector2(i, hi))
	for j: int in range(hi, lo, -2):
		inner.append(Vector2(lo, j))
	var centre: float = (lo + hi) / 2.0
	var ratio: float = maxf((3.0 * 20000.0 + 4.0 * s) / ((hi - lo) / 2.0 * s), 1.0)
	var rings: int = 4
	var positions: PackedVector3Array = PackedVector3Array()
	var normals: PackedVector3Array = PackedVector3Array()
	var custom: PackedFloat32Array = PackedFloat32Array()
	for r: int in rings + 1:
		var scale_r: float = pow(ratio, float(r) / rings) if r > 0 else 1.0
		for p: Vector2 in inner:
			positions.append(Vector3((centre + (p.x - centre) * scale_r) * s, 0.0, (centre + (p.y - centre) * scale_r) * s))
			normals.append(Vector3.UP)
			custom.append_array(PackedFloat32Array([s, 1.0, float(level), 1.0]))
	var indices: PackedInt32Array = PackedInt32Array()
	var count: int = inner.size()
	for r: int in rings:
		for k: int in count:
			var k1: int = (k + 1) % count
			var a: int = r * count + k
			var b: int = r * count + k1
			var c: int = (r + 1) * count + k1
			var d: int = (r + 1) * count + k
			for tri: Array in [[a, b, c], [a, c, d]]:
				var p0: Vector3 = positions[tri[0]]
				var p1: Vector3 = positions[tri[1]]
				var p2: Vector3 = positions[tri[2]]
				var ny: float = (p2.z - p0.z) * (p1.x - p0.x) - (p2.x - p0.x) * (p1.z - p0.z)
				indices.append_array(PackedInt32Array(tri if ny >= 0.0 else [tri[0], tri[2], tri[1]]))
	return _sea_mesh(positions, normals, custom, indices)


func _sea_mesh(positions: PackedVector3Array, normals: PackedVector3Array, custom: PackedFloat32Array, indices: PackedInt32Array) -> ArrayMesh:
	var arrays: Array = []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = positions
	arrays[Mesh.ARRAY_NORMAL] = normals
	arrays[Mesh.ARRAY_CUSTOM0] = custom
	arrays[Mesh.ARRAY_INDEX] = indices
	var mesh: ArrayMesh = ArrayMesh.new()
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays, [], {}, Mesh.ARRAY_CUSTOM_RGBA_FLOAT << Mesh.ARRAY_FORMAT_CUSTOM0_SHIFT)
	return mesh


## hull_mesh::env::ocean::snap_origins: each level snaps to multiples of twice its spacing under the camera.
func _place_sea(eye_render: Vector3) -> void:
	var levels: int = _spec[0]
	var world: Vector3 = eye_render + _origin
	for level: int in levels:
		var step: float = 2.0 * _spacing(level)
		var centre: Vector3 = Vector3(floorf(world.x / step) * step, 0.0, floorf(world.z / step) * step)
		(_sea.get_child(level) as Node3D).position = centre - _origin
	(_sea.get_child(levels) as Node3D).position = (_sea.get_child(levels - 1) as Node3D).position


# --- markers at the simulation's heights ---

## One two-tone float per fixture probe: the upper block sits on the simulation's surface height and the
## lower block hangs just under it. With the shader matching the simulation, only the upper block shows.
func _build_markers() -> void:
	_markers = Node3D.new()
	add_child(_markers)
	var top: StandardMaterial3D = StandardMaterial3D.new()
	top.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	top.albedo_color = Color(1.0, 0.45, 0.1)
	var bottom: StandardMaterial3D = StandardMaterial3D.new()
	bottom.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	bottom.albedo_color = Color(0.1, 1.0, 0.3)
	for set_name: String in _fixture["sets"]:
		var probes: Array = _fixture["sets"][set_name]["probes"]
		for i: int in probes.size():
			var probe: Array = probes[i]
			var at: Vector3 = Vector3(probe[0] - _origin.x, probe[2], probe[1] - _origin.z)
			var size: float = clampf(at.length() * 0.012, 0.12, 2.0)
			var holder: Node3D = Node3D.new()
			holder.name = set_name + "_" + str(i)
			holder.position = at
			for part: Array in [[top, 0.5], [bottom, -0.5]]:
				var box: BoxMesh = BoxMesh.new()
				box.size = Vector3(size, size * 0.5, size)
				var block: MeshInstance3D = MeshInstance3D.new()
				block.mesh = box
				block.material_override = part[0]
				block.position = Vector3(0.0, size * 0.25 * signf(part[1]), 0.0)
				block.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
				holder.add_child(block)
			_markers.add_child(holder)


# --- stand-in scenery: clouds, an island, block hulls with one hull mask ---

func _build_scenery() -> void:
	_scenery = Node3D.new()
	add_child(_scenery)
	_cloud_material = ShaderMaterial.new()
	_cloud_material.shader = load("res://shaders/cloud.gdshader")
	_island_material = ShaderMaterial.new()
	_island_material.shader = load("res://shaders/island.gdshader")
	var rng: RandomNumberGenerator = RandomNumberGenerator.new()
	rng.seed = 20261114
	# A horizon ring of six masses, then scattered cumulus (hull_mesh::env::scenery::cloud_layout in outline).
	for i: int in 6:
		var bearing: float = TAU * (i + rng.randf_range(-0.15, 0.15)) / 6.0
		var radius: float = rng.randf_range(15000.0, 17500.0)
		_add_cloud(rng, Vector3(sin(bearing) * radius, rng.randf_range(700.0, 1100.0), -cos(bearing) * radius), rng.randf_range(4200.0, 7000.0), 3)
	for i: int in 26:
		var bearing: float = rng.randf_range(0.0, TAU)
		var radius: float = rng.randf_range(2500.0, 15000.0)
		_add_cloud(rng, Vector3(sin(bearing) * radius, rng.randf_range(900.0, 1500.0), -cos(bearing) * radius), rng.randf_range(500.0, 1400.0), 1)
	_add_island(rng, Vector3(-2600.0, 0.0, -5200.0), 1500.0, 210.0)
	_add_island(rng, Vector3(5200.0, 0.0, 900.0), 900.0, 120.0)
	_add_island(rng, Vector3(-260.0, 0.0, 190.0), 150.0, 22.0)
	var haze: Color = _color(_token("hull", "token", "hull.haze"))
	var deck: Color = _color(_token("hull", "token", "hull.deck"))
	var sun_bearing: float = deg_to_rad(_sun_azimuth_deg)
	_add_hull(Vector3(sin(sun_bearing + 0.2) * 260.0, 0.0, -cos(sun_bearing + 0.2) * 260.0), 0.7, haze, deck, false)
	_add_hull(Vector3(sin(sun_bearing + 1.35) * 70.0, 0.0, -cos(sun_bearing + 1.35) * 70.0), sun_bearing + 0.3, haze, deck, true)
	_add_hull(Vector3(sin(sun_bearing + 2.9) * 420.0, 0.0, -cos(sun_bearing + 2.9) * 420.0), 2.1, haze, deck, false)


## A cumulus-like cluster of flat-shaded icospheres with the wrap and height terms of cloud_cluster.
func _add_cloud(rng: RandomNumberGenerator, at: Vector3, size: float, stacks: int) -> void:
	var puffs: Array[Array] = []
	for stack: int in stacks:
		var along: float = (stack - (stacks - 1) * 0.5) * size * 0.3
		var d: float = size / maxf(1.0, stacks * 0.6)
		var r0: float = d * 0.27
		puffs.append([Vector3(along, r0 * 0.6, 0.0), Vector3(r0, minf(r0, r0 * 0.63), r0), 1])
		var n: int = rng.randi_range(5, 8)
		for i: int in n:
			var a: float = TAU * i / n + rng.randf_range(-0.3, 0.3)
			var r: float = d * rng.randf_range(0.13, 0.2)
			var dist: float = minf(d * rng.randf_range(0.18, 0.3), d * 0.5 - r)
			var cy: float = r * rng.randf_range(0.45, 0.8)
			puffs.append([Vector3(along + dist * cos(a), cy, dist * sin(a)), Vector3(r, clampf(cy + 0.03 * r, r * 0.35, r), r), 1 if r > d * 0.17 else 0])
		for i: int in rng.randi_range(1, 3):
			var a: float = rng.randf_range(0.0, TAU)
			var r: float = d * rng.randf_range(0.12, 0.17)
			var off: float = d * rng.randf_range(0.0, 0.1)
			puffs.append([Vector3(along + off * cos(a), r0 * 1.15 + r * rng.randf_range(0.1, 0.5), off * sin(a)), Vector3(r, r * 0.9, r), 1])
	var positions: PackedVector3Array = PackedVector3Array()
	var normals: PackedVector3Array = PackedVector3Array()
	for puff: Array in puffs:
		_icosphere(positions, normals, puff[0], puff[1], puff[2], rng.randf_range(0.0, TAU))
	var box: AABB = AABB(positions[0], Vector3.ZERO)
	for p: Vector3 in positions:
		box = box.expand(p)
	var colors: PackedColorArray = PackedColorArray()
	var custom: PackedFloat32Array = PackedFloat32Array()
	var centre: Vector3 = box.get_center()
	for p: Vector3 in positions:
		var h: float = clampf(p.y / maxf(box.end.y, 0.001), 0.0, 1.0)
		var edge: float = clampf(Vector2((p.x - centre.x) / (0.5 * box.size.x), (p.z - centre.z) / (0.5 * box.size.z)).length(), 0.0, 1.0)
		colors.append(Color.WHITE)
		custom.append_array(PackedFloat32Array([clampf(0.2 + 0.4 * h + 0.2 * edge, 0.2, 0.8), h, 0.0, 0.0]))
	var arrays: Array = []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = positions
	arrays[Mesh.ARRAY_NORMAL] = normals
	arrays[Mesh.ARRAY_COLOR] = colors
	arrays[Mesh.ARRAY_CUSTOM0] = custom
	var mesh: ArrayMesh = ArrayMesh.new()
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays, [], {}, Mesh.ARRAY_CUSTOM_RGBA_FLOAT << Mesh.ARRAY_FORMAT_CUSTOM0_SHIFT)
	var instance: MeshInstance3D = MeshInstance3D.new()
	instance.mesh = mesh
	instance.material_override = _cloud_material
	instance.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	instance.position = at
	instance.rotation.y = rng.randf_range(0.0, TAU)
	_scenery.add_child(instance)


## Flat-shaded icosphere (subdivision 0 or 1) scaled to an ellipsoid, clockwise front faces.
func _icosphere(positions: PackedVector3Array, normals: PackedVector3Array, centre: Vector3, radii: Vector3, subdiv: int, yaw: float) -> void:
	var t: float = (1.0 + sqrt(5.0)) * 0.5
	var verts: Array[Vector3] = [Vector3(-1, t, 0), Vector3(1, t, 0), Vector3(-1, -t, 0), Vector3(1, -t, 0), Vector3(0, -1, t), Vector3(0, 1, t), Vector3(0, -1, -t), Vector3(0, 1, -t), Vector3(t, 0, -1), Vector3(t, 0, 1), Vector3(-t, 0, -1), Vector3(-t, 0, 1)]
	for i: int in verts.size():
		verts[i] = verts[i].normalized()
	var faces: Array[Vector3i] = [Vector3i(0, 11, 5), Vector3i(0, 5, 1), Vector3i(0, 1, 7), Vector3i(0, 7, 10), Vector3i(0, 10, 11), Vector3i(1, 5, 9), Vector3i(5, 11, 4), Vector3i(11, 10, 2), Vector3i(10, 7, 6), Vector3i(7, 1, 8), Vector3i(3, 9, 4), Vector3i(3, 4, 2), Vector3i(3, 2, 6), Vector3i(3, 6, 8), Vector3i(3, 8, 9), Vector3i(4, 9, 5), Vector3i(2, 4, 11), Vector3i(6, 2, 10), Vector3i(8, 6, 7), Vector3i(9, 8, 1)]
	var tris: Array[Array] = []
	for f: Vector3i in faces:
		var a: Vector3 = verts[f.x]
		var b: Vector3 = verts[f.y]
		var c: Vector3 = verts[f.z]
		if subdiv > 0:
			var ab: Vector3 = (a + b).normalized()
			var bc: Vector3 = (b + c).normalized()
			var ca: Vector3 = (c + a).normalized()
			tris.append_array([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]])
		else:
			tris.append([a, b, c])
	var basis: Basis = Basis(Vector3.UP, yaw) * Basis.from_scale(radii)
	for tri: Array in tris:
		var p0: Vector3 = centre + basis * (tri[0] as Vector3)
		var p1: Vector3 = centre + basis * (tri[1] as Vector3)
		var p2: Vector3 = centre + basis * (tri[2] as Vector3)
		var normal: Vector3 = (p2 - p0).cross(p1 - p0).normalized()
		var outward: Vector3 = (p0 + p1 + p2) / 3.0 - centre
		if normal.dot(outward) < 0.0:
			var swap: Vector3 = p1
			p1 = p2
			p2 = swap
			normal = -normal
		positions.append_array(PackedVector3Array([p0, p1, p2]))
		normals.append_array(PackedVector3Array([normal, normal, normal]))


## A cone-shaped island of flat triangles with the token height bands.
func _add_island(rng: RandomNumberGenerator, at: Vector3, radius: float, peak: float) -> void:
	var noise: FastNoiseLite = FastNoiseLite.new()
	noise.seed = rng.randi()
	noise.frequency = 2.2 / radius
	noise.fractal_octaves = 4
	var edge: float = maxf(8.0, radius / 28.0)
	var half: int = int(ceil(radius * 1.25 / edge))
	var bands: Array = _tokens["island_bands"]
	var positions: PackedVector3Array = PackedVector3Array()
	var normals: PackedVector3Array = PackedVector3Array()
	var colors: PackedColorArray = PackedColorArray()
	_islands.append([at, radius, peak, noise])
	var island: int = _islands.size() - 1
	for j: int in range(-half, half):
		for i: int in range(-half, half):
			var corners: Array[Vector3] = []
			for o: Vector2i in [Vector2i(0, 0), Vector2i(1, 0), Vector2i(0, 1), Vector2i(1, 1)]:
				var x: float = (i + o.x) * edge
				var z: float = (j + o.y) * edge
				corners.append(Vector3(x, _island_height(island, x, z), z))
			for tri: Array in [[0, 1, 2], [1, 3, 2]]:
				var p0: Vector3 = corners[tri[0]]
				var p1: Vector3 = corners[tri[1]]
				var p2: Vector3 = corners[tri[2]]
				if maxf(p0.y, maxf(p1.y, p2.y)) < -2.5:
					continue
				var normal: Vector3 = (p2 - p0).cross(p1 - p0).normalized()
				var mean: float = (p0.y + p1.y + p2.y) / 3.0
				var color: Color = Color.BLACK
				for band: Dictionary in bands:
					var lin: Array = band["linear_rgb"]
					color = Color(lin[0], lin[1], lin[2])
					if mean <= float(band["top_m"]):
						break
				positions.append_array(PackedVector3Array([p0, p1, p2]))
				normals.append_array(PackedVector3Array([normal, normal, normal]))
				colors.append_array(PackedColorArray([color, color, color]))
	var arrays: Array = []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = positions
	arrays[Mesh.ARRAY_NORMAL] = normals
	arrays[Mesh.ARRAY_COLOR] = colors
	var mesh: ArrayMesh = ArrayMesh.new()
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	var instance: MeshInstance3D = MeshInstance3D.new()
	instance.mesh = mesh
	instance.material_override = _island_material
	instance.position = at
	_scenery.add_child(instance)


## Height of island `index` at a position relative to its centre, metres above mean sea level.
func _island_height(index: int, x: float, z: float) -> float:
	var radius: float = _islands[index][1]
	var peak: float = _islands[index][2]
	var noise: FastNoiseLite = _islands[index][3]
	var r: float = Vector2(x, z).length() / radius
	return peak * (maxf(1.0 - r, 0.0) ** 1.4) * (0.55 + 0.9 * (noise.get_noise_2d(x, z) * 0.5 + 0.5)) - 3.0 * smoothstep(0.75, 1.1, r) - 0.6 - 40.0 * maxf(r - 1.0, 0.0)


## The water's shore heightfield: ground height around the render origin, here 2048 m at 8 m per texel
## in a float image (the game's is R16 at 4 m; shore_decode covers the difference).
func _build_shore() -> void:
	var size: int = 256
	var extent: float = 2048.0
	var image: Image = Image.create(size, size, false, Image.FORMAT_RF)
	for v: int in size:
		for u: int in size:
			var x: float = (u + 0.5) / size * extent - 0.5 * extent
			var z: float = (v + 0.5) / size * extent - 0.5 * extent
			var ground: float = -60.0
			for i: int in _islands.size():
				var centre: Vector3 = _islands[i][0]
				if Vector2(x - centre.x, z - centre.z).length() < 2.5 * float(_islands[i][1]):
					ground = maxf(ground, _island_height(i, x - centre.x, z - centre.z))
			image.set_pixel(u, v, Color(ground, 0.0, 0.0))
	var texture: ImageTexture = ImageTexture.create_from_image(image)
	_water.set_shader_parameter(&"shore_height", texture)
	_water.set_shader_parameter(&"shore_window", Vector4(-0.5 * extent, -0.5 * extent, 1.0 / extent, 0.0))
	_water.set_shader_parameter(&"shore_decode", Vector2(1.0, 0.0))
	_water.set_shader_parameter(&"shore_enabled", true)


## A 90 m block hull: grey boxes in token paint. With `masked`, it also feeds the water's hull mask
## (layer 0): the dry column, and the distance to the waterline outline for contact foam.
func _add_hull(at: Vector3, yaw: float, side: Color, deck: Color, masked: bool) -> void:
	var hull: Node3D = Node3D.new()
	hull.position = at
	hull.rotation.y = yaw
	_scenery.add_child(hull)
	_hulls.append(hull)
	var paint: StandardMaterial3D = StandardMaterial3D.new()
	paint.albedo_color = side
	paint.roughness = 0.75
	var dark: StandardMaterial3D = StandardMaterial3D.new()
	dark.albedo_color = deck
	dark.roughness = 0.8
	# Hull-local frame of hull_mesh::mask: x from the stern to the bow, z to starboard, y up from the waterline.
	var blocks: Array[Array] = [
		[Vector3(45.0, 0.5, 0.0), Vector3(90.0, 9.0, 12.0), paint],
		[Vector3(45.0, 5.05, 0.0), Vector3(90.0, 0.1, 12.0), dark],
		[Vector3(40.0, 8.0, 0.0), Vector3(22.0, 6.0, 8.0), paint],
		[Vector3(43.0, 13.0, 0.0), Vector3(8.0, 4.0, 6.0), paint],
		[Vector3(40.0, 19.0, 0.0), Vector3(1.0, 10.0, 1.0), paint],
		[Vector3(68.0, 6.5, 0.0), Vector3(8.0, 3.0, 5.0), paint],
		[Vector3(16.0, 6.5, 0.0), Vector3(8.0, 3.0, 5.0), paint],
	]
	for block: Array in blocks:
		var box: BoxMesh = BoxMesh.new()
		box.size = block[1]
		var instance: MeshInstance3D = MeshInstance3D.new()
		instance.mesh = box
		instance.material_override = block[2]
		instance.position = block[0]
		hull.add_child(instance)
	if not masked:
		return
	var image: Image = Image.create(512, 128, false, Image.FORMAT_RGBAH)
	for v: int in 128:
		for u: int in 512:
			var x: float = (u + 0.5) * 0.5
			var z: float = (v + 0.5 - 64.0) * 0.5
			var outside: Vector2 = Vector2(absf(x - 45.0) - 45.0, absf(z) - 6.0)
			var distance: float = Vector2(maxf(outside.x, 0.0), maxf(outside.y, 0.0)).length() + minf(maxf(outside.x, outside.y), 0.0)
			image.set_pixel(u, v, Color(-4.0, 5.0, distance, 1.0) if distance < 0.0 else Color(30000.0, -30000.0, distance, 0.0))
	var masks: Texture2DArray = Texture2DArray.new()
	masks.create_from_images([image])
	var from_render: Array[Projection] = []
	from_render.append(Projection(hull.global_transform.affine_inverse()))
	for i: int in 7:
		from_render.append(Projection.IDENTITY)
	_water.set_shader_parameter(&"hull_masks", masks)
	_water.set_shader_parameter(&"hull_mask_from_render", from_render)
	_water.set_shader_parameter(&"hull_count", 1)


# --- frame-time cost of the sea ---

## Mean GPU-bound frame time of one shot with and without the sea, at the window's size times `scale_3d`.
func _bench(scale_3d: float, shot_index: int) -> void:
	get_viewport().scaling_3d_scale = scale_3d
	_apply_shot(SHOTS[shot_index])
	var results: Dictionary = {}
	for mode: String in ["sea", "no_sea", "sea_again"]:
		_sea.visible = mode != "no_sea"
		await _draw_frames(30)
		# Presenting locks each frame to the display, so the timed frames are drawn without a swap and the
		# clock stops when a read-back has waited for the GPU to finish them all.
		get_viewport().get_texture().get_image()
		var start: int = Time.get_ticks_usec()
		var frames: int = 300
		for i: int in frames:
			RenderingServer.force_draw(false, 1.0 / 60.0)
		get_viewport().get_texture().get_image()
		results[mode] = (Time.get_ticks_usec() - start) / 1000.0 / frames
	var size: Vector2i = get_viewport().get_visible_rect().size
	print("sea_capture bench: ", SHOTS[shot_index]["name"], " ", size.x, "x", size.y, " at 3D scale ", scale_3d, ", ms per frame ", results, "; sea costs ", snappedf(0.5 * (results["sea"] + results["sea_again"]) - results["no_sea"], 0.001), " ms")
