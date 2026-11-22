extends Node3D
## Main-menu backdrop: open sea at dusk behind the menu column. DESIGN.md L1855-1883 (dusk, the low
## warm sun as hero light), L1885-1917 (the lit sea), L1594 (menu text stays on an opaque panel).
## Reads and calls: Session.state, Session.state_changed, Settings.changed, Ui.reduce_motion.
##
## No simulation runs behind the menu, so this is the dry-dock studio's recipe turned to face the
## sun: the dusk-clear preset of the world tokens (sky shader, fog, ambient, AgX and the post stack,
## through res://world/environment.gd), the sun a few degrees over the horizon, and calm water on
## res://shaders/water.gdshader with the baked ripple detail and no waves. The camera stands a few metres over the water looking a little to one side of the sun, so
## the glitter path runs up the open right side of the screen, clear of the menu panel. Nothing in
## the picture is a ship: every ship shown in the game is a saved design (L1601), and the menu has
## none loaded.
##
## The only motion is the ripple detail, the same 24 s loop the dock uses; under reduce motion the
## water holds still. The camera never moves. While a session loads or runs (a replay opened from
## the command line, a sortie started from a setup screen) the sortie world owns the 3D, so the
## stage turns its environment, light and water off, as the dock stage does. The menu hides the
## whole stage while another screen covers it.
##
## There is no quality profile node (res://world/quality.gd) here: its exit resets the viewport's 3D
## scaling, and the menu leaves the tree after the dry dock or sortie that replaces it has applied
## its own profile. A flat calm sea needs no tier settings of its own.

## Sun: low in the dusk band (L1868: dusk is 3 to under 25 degrees), warm and long-shadowed.
const SUN_ELEVATION_DEG := 3.5
## Compass bearing the camera looks along, and the sun's bearing: the sun sits right of centre.
const VIEW_AZIMUTH_DEG := 250.0
const SUN_AZIMUTH_DEG := 262.0
## Camera height over the water and downward pitch, metres and degrees.
const CAMERA_HEIGHT_M := 4.0
const CAMERA_PITCH_DEG := -3.0
const CAMERA_FOV_DEG := 55.0
const DETAIL_STRENGTH := 0.35
const DETAIL_LOOP_S := 24.0
const WATER_SHADER := "res://shaders/water.gdshader"
const WorldLook := preload("res://world/environment.gd")
const DockStage := preload("res://world/dock_stage.gd")

@onready var camera: Camera3D = $Camera
@onready var _environment: WorldEnvironment = $Environment
@onready var _key: DirectionalLight3D = $Key
@onready var _water: MeshInstance3D = $Water
@onready var _presets: Node = $Presets

var _env_resource: Environment
var _water_material: ShaderMaterial
var _active: bool = true
var _still: bool = false
var _in_session: bool = false
var _covered: bool = false


func _ready() -> void:
	_env_resource = _environment.environment
	_presets.apply_static(Bp.TIME_DUSK, Bp.WEATHER_CLEAR, SUN_AZIMUTH_DEG, SUN_ELEVATION_DEG)
	camera.fov = CAMERA_FOV_DEG
	var yaw := deg_to_rad(-VIEW_AZIMUTH_DEG)
	camera.transform = Transform3D(Basis.from_euler(Vector3(deg_to_rad(CAMERA_PITCH_DEG), yaw, 0.0), EULER_ORDER_YXZ), Vector3(0.0, CAMERA_HEIGHT_M, 0.0))
	camera.make_current()
	_build_water()
	Session.state_changed.connect(_on_session_state)
	var ui := get_node_or_null(^"/root/Ui")
	if ui != null and ui.has_signal(&"changed"):
		ui.connect(&"changed", _on_ui_changed)
	_on_ui_changed(&"")
	_on_session_state(Session.state())


func _process(_delta: float) -> void:
	if not _active or _still or _water_material == null or not is_visible_in_tree():
		return
	var t := Time.get_ticks_msec() / 1000.0
	_water_material.set_shader_parameter(&"detail_phase", fposmod(t / DETAIL_LOOP_S, 1.0))


func _build_water() -> void:
	var shader := load(WATER_SHADER) as Shader
	if shader == null:
		push_warning("menu stage: %s did not load (run cargo xtask gen); no water" % WATER_SHADER)
		_water.visible = false
		return
	_water_material = ShaderMaterial.new()
	_water_material.shader = shader
	for token: String in ["deep", "scatter", "shallow", "foam"]:
		var c := WorldLook.sea_token("sea." + token)
		_water_material.set_shader_parameter(StringName(token + "_color"), WorldLook.srgb(c))
	_water_material.set_shader_parameter(&"sea_state", 0.0)
	_water_material.set_shader_parameter(&"rain", 0.0)
	_water_material.set_shader_parameter(&"nyquist_fade", 1.0)
	_water_material.set_shader_parameter(&"hull_count", 0)
	_water_material.set_shader_parameter(&"shore_enabled", false)
	_water_material.set_shader_parameter(&"detail_tilings", 2)
	var detail := DockStage._detail_arrays()
	if detail.size() == 2:
		_water_material.set_shader_parameter(&"detail_normals", detail[0])
		_water_material.set_shader_parameter(&"detail_turbulence", detail[1])
		_water_material.set_shader_parameter(&"detail_strength", DETAIL_STRENGTH)
	else:
		_water_material.set_shader_parameter(&"detail_strength", 0.0)
	_water.material_override = _water_material
	_water.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF


func _on_ui_changed(_what: StringName) -> void:
	var ui := get_node_or_null(^"/root/Ui")
	_still = ui != null and bool(ui.get(&"reduce_motion"))


func _on_session_state(state: int) -> void:
	_in_session = state == Bp.SESSION_LOADING or state == Bp.SESSION_IN_SORTIE or state == Bp.SESSION_ENDED or state == Bp.SESSION_REPLAY
	_refresh()


## Another screen covers the menu (settings, credits, replays are opaque): stop drawing the sea.
func set_covered(covered: bool) -> void:
	_covered = covered
	_refresh()


## On: this stage's environment, light and water draw. Off: they step aside for a sortie world, or
## because nothing of them would show.
func _refresh() -> void:
	var on := not _in_session and not _covered
	if on == _active:
		return
	_active = on
	_environment.environment = _env_resource if on else null
	_key.visible = on
	_water.visible = on and _water_material != null
	if on:
		camera.make_current()
