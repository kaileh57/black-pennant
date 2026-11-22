extends Node3D
## Dry-dock stage: BpEditorView draws the design; this script binds the camera.
## DESIGN.md L168, L1940. Unit: world-dock.
## Reads and calls: BpEditorView.
##
## The stage is a studio at dusk: the dusk-clear preset of the world tokens (sky, fog, ambient, exposure,
## AgX and the post stack, through res://world/environment.gd and res://world/quality.gd) with the key light
## raised to the top of the dusk band and swung onto the starboard bow, so decks catch light and the side
## the dock opens on reads in light, with a cool sky fill in its shadows. The design floats on calm water: a
## flat plane on res://shaders/water.gdshader at the design waterline (`Editor.hull().waterline_m`), with
## the baked ripple detail at the dock's quarter strength and no waves, so the boot-topping band and the
## antifouling below it read against the water as they will at sea.
##
## Depth of field is allowed here (L1824) and follows the accessibility blur slider (`access.blur`, default
## 0, L1718): far blur only, beginning past the whole design, so the hull is always sharp and only the
## horizon softens. The dock has no lightning, weather or night.
##
## While a session loads or runs (a splash, a sortie), the sortie world owns the 3D: the stage turns its
## environment, light and water off so the two scenes never stack, and back on when the session ends.
## Reads and calls: BpEditorView.bind_camera, Editor.hull, Editor.design_changed, Session.state,
## Session.state_changed, Settings.get_value, Settings.changed.

## Key light: dusk, near the top of the band (L1868: dusk is 3 to under 25 degrees) so decks are lit.
const KEY_ELEVATION_DEG := 20.0
## Compass bearing of the key in the dock frame (the design's bow is +X, starboard +Z, north -Z): over the
## starboard bow.
const KEY_AZIMUTH_DEG := 150.0
## Baked ripple detail in the dock (water.gdshader: 0.25 in the dry dock) and its 24 s loop.
const DETAIL_STRENGTH := 0.25
const DETAIL_LOOP_S := 24.0
const DETAIL_TILINGS: Array[float] = [41.0, 11.3]
const DETAIL_NORMALS := "res://art/gen/ocean_detail_normals.bin"
const DETAIL_TURBULENCE := "res://art/gen/ocean_detail_turbulence.bin"
const WATER_SHADER := "res://shaders/water.gdshader"
const CELL_M := 0.5
const WorldLook := preload("res://world/environment.gd")
## Far depth of field at full slider: blur amount, and how far past the design's far side it starts and
## ramps, as multiples of the design's half-diagonal.
const DOF_AMOUNT := 0.12
const DOF_START_RADII := 1.5
const DOF_RAMP_RADII := 6.0

## Detail texture arrays, shared by every stage opened in this run (8 MB, read once).
static var _detail: Array[Texture2DArray] = []
static var _detail_tried: bool = false

@onready var view: BpEditorView = $View
@onready var camera: Camera3D = $Camera
@onready var _environment: WorldEnvironment = $Environment
@onready var _key: DirectionalLight3D = $Key
@onready var _water: MeshInstance3D = $Water
@onready var _presets: Node = $Presets

var _env_resource: Environment
var _water_material: ShaderMaterial
var _active: bool = true
var _centre: Vector3 = Vector3.ZERO
var _radius_m: float = 20.0
var _blur: float = 0.0


func _ready() -> void:
	view.bind_camera(camera)
	_env_resource = _environment.environment
	_presets.apply_static(Bp.TIME_DUSK, Bp.WEATHER_CLEAR, KEY_AZIMUTH_DEG, KEY_ELEVATION_DEG)
	_build_water()
	Editor.design_changed.connect(_on_design_changed)
	Settings.changed.connect(_on_setting_changed)
	Session.state_changed.connect(_on_session_state)
	_blur = clampf(float(Settings.get_value(&"access.blur")), 0.0, 1.0)
	_on_design_changed(0)
	_on_session_state(Session.state())


func _process(_delta: float) -> void:
	if not _active:
		return
	if _water_material != null:
		var t := Time.get_ticks_msec() / 1000.0
		_water_material.set_shader_parameter(&"detail_phase", fposmod(t / DETAIL_LOOP_S, 1.0))
	if _blur > 0.0:
		_update_dof()


func _build_water() -> void:
	var shader := load(WATER_SHADER) as Shader
	if shader == null:
		push_warning("dock stage: %s did not load (run cargo xtask gen); no water" % WATER_SHADER)
		_water.visible = false
		return
	_water_material = ShaderMaterial.new()
	_water_material.shader = shader
	for token: String in ["deep", "scatter", "shallow", "foam"]:
		var c := WorldLook.sea_token("sea." + token)
		_water_material.set_shader_parameter(StringName(token + "_color"), WorldLook.srgb(c))
	# Calm: no whitecaps, no rain, every wave component kept (the dock has none).
	_water_material.set_shader_parameter(&"sea_state", 0.0)
	_water_material.set_shader_parameter(&"rain", 0.0)
	_water_material.set_shader_parameter(&"nyquist_fade", 1.0)
	_water_material.set_shader_parameter(&"hull_count", 0)
	_water_material.set_shader_parameter(&"shore_enabled", false)
	_water_material.set_shader_parameter(&"detail_tilings", 2)
	var detail := _detail_arrays()
	if detail.size() == 2:
		_water_material.set_shader_parameter(&"detail_normals", detail[0])
		_water_material.set_shader_parameter(&"detail_turbulence", detail[1])
		_water_material.set_shader_parameter(&"detail_strength", DETAIL_STRENGTH)
	else:
		_water_material.set_shader_parameter(&"detail_strength", 0.0)
	_water.material_override = _water_material
	_water.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF


## The baked sea detail as two texture arrays (normals, turbulence), or an empty array when
## `cargo xtask gen` has not written them.
static func _detail_arrays() -> Array[Texture2DArray]:
	if _detail_tried:
		return _detail
	_detail_tried = true
	var normals := _load_blob(DETAIL_NORMALS)
	var turbulence := _load_blob(DETAIL_TURBULENCE)
	if normals.is_empty() or turbulence.is_empty():
		push_warning("dock stage: the baked sea detail is missing (run cargo xtask gen); the water draws without ripples")
		return _detail
	var out: Array[Texture2DArray] = []
	for layers: Array[Image] in [normals, turbulence]:
		var array := Texture2DArray.new()
		if array.create_from_images(layers) != OK:
			return _detail
		out.append(array)
	_detail = out
	return _detail


## Layers of a hull_mesh::gen_files::TexBlob file ("BPTX", version 1, kind, Godot image format, width,
## height, layers, mip levels; then each layer with its mip chain) as images; empty when missing or bad.
static func _load_blob(path: String) -> Array[Image]:
	var images: Array[Image] = []
	if not FileAccess.file_exists(path):
		return images
	var bytes := FileAccess.get_file_as_bytes(path)
	if bytes.size() < 32 or bytes.slice(0, 4).get_string_from_ascii() != "BPTX" or bytes.decode_u32(4) != 1:
		return images
	var format := bytes.decode_u32(12)
	var width := bytes.decode_u32(16)
	var height := bytes.decode_u32(20)
	var layers := bytes.decode_u32(24)
	var mips := bytes.decode_u32(28)
	if layers <= 0 or width <= 0 or height <= 0 or format >= Image.FORMAT_MAX or (bytes.size() - 32) % layers != 0:
		return images
	var per_layer := (bytes.size() - 32) / layers
	if per_layer != Image.create_empty(width, height, mips > 1, format as Image.Format).get_data().size():
		return images
	for i: int in layers:
		var data := bytes.slice(32 + i * per_layer, 32 + (i + 1) * per_layer)
		var image := Image.create_from_data(width, height, mips > 1, format as Image.Format, data)
		if image == null or image.is_empty():
			return [] as Array[Image]
		images.append(image)
	return images


## Follow the design: the water stands at its design waterline, and the depth of field frames its size.
func _on_design_changed(_rev: int) -> void:
	var hull: Dictionary = Editor.hull() if Editor.is_open() else {}
	if hull.is_empty():
		return
	var waterline := float(hull.get("waterline_m", 0.0))
	var size_v: Variant = hull.get("size_cells", Vector3i.ZERO)
	var size: Vector3 = Vector3(size_v as Vector3i) * CELL_M if size_v is Vector3i else Vector3.ZERO
	_centre = size * 0.5
	_radius_m = maxf(size.length() * 0.5, 5.0)
	if is_finite(waterline):
		_water.position = Vector3(_centre.x, waterline, _centre.z)


func _on_setting_changed(key: StringName) -> void:
	if key == &"access.blur":
		_blur = clampf(float(Settings.get_value(&"access.blur")), 0.0, 1.0)
		_update_dof()


## Far blur past the whole design, scaled by the blur slider; off at 0.
func _update_dof() -> void:
	var attributes := camera.attributes as CameraAttributesPractical
	if attributes == null:
		return
	attributes.dof_blur_near_enabled = false
	attributes.dof_blur_far_enabled = _blur > 0.0 and _active
	if not attributes.dof_blur_far_enabled:
		return
	var reach := camera.global_position.distance_to(to_global(_centre)) + _radius_m * DOF_START_RADII
	attributes.dof_blur_far_distance = reach
	attributes.dof_blur_far_transition = _radius_m * DOF_RAMP_RADII
	attributes.dof_blur_amount = DOF_AMOUNT * _blur


func _on_session_state(state: int) -> void:
	var sortie := state == Bp.SESSION_LOADING or state == Bp.SESSION_IN_SORTIE or state == Bp.SESSION_ENDED or state == Bp.SESSION_REPLAY
	_set_active(not sortie)


## On: this stage's environment, key light and water draw. Off: they step aside for the sortie world.
func _set_active(on: bool) -> void:
	if on == _active:
		return
	_active = on
	_environment.environment = _env_resource if on else null
	_key.visible = on
	_water.visible = on and _water_material != null
	_update_dof()
