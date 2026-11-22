extends Node3D
## Sortie world: BpWorldView draws the simulation; this script binds the camera and applies environment presets.
## DESIGN.md L1826-1883. Unit: world-scene.
## Reads and calls: BpWorldView.environment, BpWorldView.environment_changed, BpWorldView.lightning, Quality.profile.
##
## The scene: View (BpWorldView), Camera (60 degrees, far 20,000 m, moved by the view), Environment
## (WorldEnvironment: AgX, sky shader, fog, glow on emission only), Sun then Moon (the sky shader's LIGHT0 and
## LIGHT1), Presets (res://world/environment.gd) and Profile (res://world/quality.gd).
##
## This script only wires them: it binds the camera, shows the dusk-clear preset until the sortie's first
## weather event arrives (behind the loading cover), hands the view to the presets, and asks the view to
## warm up every material each time a session starts loading, so Screens can lift the loading cover on
## `warmed_up`. Camera input belongs to the sortie HUD; the camera has no attributes, so there is no depth
## of field and no auto exposure in a sortie (L1824, L1848).

## Where the sun stands before the first weather event (it is behind the loading cover then).
const OPENING_SUN_AZIMUTH_DEG := 250.0

@onready var view: BpWorldView = $View
@onready var camera: Camera3D = $Camera
@onready var presets: Node = $Presets

var _warm_asked: bool = false


func _ready() -> void:
	view.bind_camera(camera)
	camera.make_current()
	presets.apply_static(Bp.TIME_DUSK, Bp.WEATHER_CLEAR, OPENING_SUN_AZIMUTH_DEG)
	presets.bind_view(view)
	Session.state_changed.connect(_on_session_state)
	_on_session_state(Session.state())


func _on_session_state(state: int) -> void:
	match state:
		Bp.SESSION_LOADING, Bp.SESSION_IN_SORTIE, Bp.SESSION_REPLAY:
			if not _warm_asked:
				_warm_asked = true
				view.warm_up()
		Bp.SESSION_IDLE, Bp.SESSION_ENDED, Bp.SESSION_FAILED:
			# The next session in this world warms up again (a splash restarted from the dock, a replay).
			_warm_asked = false
		_:
			pass
