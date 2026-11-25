extends Control
## Kill-cam frame shown over the sortie after the own hull is lost.
## DESIGN.md L1049. Unit: ui-hud-shell.
## Reads and calls: Session.kill_cam_ready, Session.kill_cam_play.
##
## When `Session.kill_cam_ready(clip)` reports a clip (`{ victim, killer, from_s, to_s }`), a panel
## at the bottom centre says the hull was lost, by which track when the killer is one the side
## holds, and offers to watch the clip or dismiss it. Watching plays it over the live sortie
## (`Session.kill_cam_play`) with the orbit camera on the vessel that fired (the victim when the
## killer is not drawn), a progress bar on sortie time, and a button back to the live picture. At
## the end of the clip the live picture returns and the panel offers to watch again. In PvP the
## clip shows only what the killed player's side detected; the session enforces that (L1049).
## Hidden in every other state; the session going idle or loading closes it.

const Kit := preload("res://ui/screens/menu_kit.gd")

## Progress is read from the session clock this often while the clip plays.
const POLL_S := 0.25
## The clip has ended once sortie time is this close to its end.
const END_SLACK_S := 0.05

@onready var _panel: PanelContainer = $Panel
@onready var _margin: MarginContainer = $Panel/Margin
@onready var _column: VBoxContainer = $Panel/Margin/Column
@onready var _title: Label = $Panel/Margin/Column/Title
@onready var _body: Label = $Panel/Margin/Column/Body
@onready var _progress: ProgressBar = $Panel/Margin/Column/Progress
@onready var _buttons: HBoxContainer = $Panel/Margin/Column/Buttons
@onready var _watch: Button = $Panel/Margin/Column/Buttons/Watch
@onready var _dismiss: Button = $Panel/Margin/Column/Buttons/Dismiss

var _clip: Dictionary = {}
var _playing := false
var _played := false
var _world_ref: WeakRef = null
var _timer: Timer


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_margins(_margin, 3)
	Ui.set_separation(_column, 2)
	Ui.set_separation(_buttons, 2)
	_body.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_watch.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_dismiss.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_panel.accessibility_name = tr(&"hud_shell.kill_cam.title")
	_watch.pressed.connect(_on_watch)
	_dismiss.pressed.connect(_on_dismiss)
	Kit.chain_horizontal([_watch, _dismiss])
	_timer = Timer.new()
	_timer.wait_time = POLL_S
	_timer.timeout.connect(_poll)
	add_child(_timer)
	var session := Kit.autoload(&"Session")
	if session != null:
		if session.has_signal(&"kill_cam_ready"):
			session.connect(&"kill_cam_ready", show_clip)
		if session.has_signal(&"state_changed"):
			session.connect(&"state_changed", _on_state)
	_render()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render()


## The world view whose camera follows the clip (the HUD shell hands it over).
func set_world(view: Node) -> void:
	_world_ref = weakref(view) if view != null else null


## Offer a clip (the Dictionary from `Session.kill_cam_ready`). A malformed clip is ignored.
func show_clip(clip: Dictionary) -> void:
	var from_s := Kit.get_float(clip, "from_s", -1.0)
	var to_s := Kit.get_float(clip, "to_s", -1.0)
	if from_s < 0.0 or to_s < from_s:
		return
	if _playing:
		_stop()
	_clip = clip
	_played = false
	_render()
	if Ui.using_gamepad:
		Ui.focus(_watch)


func is_open() -> bool:
	return not _clip.is_empty()


func _on_state(state: int) -> void:
	if state != Bp.SESSION_IN_SORTIE and state != Bp.SESSION_ENDED:
		if _playing:
			_stop()
		_clip = {}
		_render()


func _on_watch() -> void:
	if _clip.is_empty():
		return
	if _playing:
		_stop()
		_render()
		return
	if Kit.call_on(&"Session", &"kill_cam_play", [_clip]) != true:
		_body.text = tr(&"hud_shell.kill_cam.unavailable")
		return
	_playing = true
	_follow(Kit.get_int(_clip, "killer", Bp.NONE), Kit.get_int(_clip, "victim", Bp.NONE))
	_timer.start()
	_render()


func _on_dismiss() -> void:
	if _playing:
		_stop()
	_clip = {}
	_render()


func _stop() -> void:
	_playing = false
	_played = true
	_timer.stop()
	Kit.call_on(&"Session", &"kill_cam_stop")
	_follow(Bp.NONE, Bp.NONE)


func _follow(first: int, second: int) -> void:
	var w: Object = _world_ref.get_ref() if _world_ref != null else null
	if w == null or not is_instance_valid(w) or not (w as Node).has_method(&"follow"):
		return
	if first != Bp.NONE and (w as Node).call(&"follow", first) == true:
		return
	if second != Bp.NONE and (w as Node).call(&"follow", second) == true:
		return
	(w as Node).call(&"follow", Bp.NONE)


func _poll() -> void:
	if not _playing:
		return
	var from_s := Kit.get_float(_clip, "from_s", 0.0)
	var to_s := Kit.get_float(_clip, "to_s", 0.0)
	var t: Variant = Kit.call_on(&"Session", &"time_s")
	var now := float(t) if t is float else from_s
	var span := maxf(to_s - from_s, 0.001)
	_progress.value = clampf((now - from_s) / span, 0.0, 1.0)
	if now >= to_s - END_SLACK_S:
		_stop()
		_render()


func _killer_text() -> String:
	var killer := Kit.get_int(_clip, "killer", Bp.NONE)
	if killer == Bp.NONE:
		return ""
	for t: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"tracks")):
		if Kit.get_int(t, "vessel", Bp.NONE) == killer:
			return tr(&"hud_shell.kill_cam.by_track").format([Kit.get_str(t, "label")])
	return ""


func _render() -> void:
	if not is_node_ready():
		return
	_panel.visible = not _clip.is_empty()
	if _clip.is_empty():
		return
	var span := maxf(Kit.get_float(_clip, "to_s", 0.0) - Kit.get_float(_clip, "from_s", 0.0), 0.0)
	var duration := String(Kit.call_on(&"Tx", &"duration", [span])) if Kit.autoload(&"Tx") != null else str(roundi(span))
	var by := _killer_text()
	if _playing:
		_title.text = &"hud_shell.kill_cam.playing_title"
		_body.text = by if not by.is_empty() else tr(&"hud_shell.kill_cam.playing")
		_watch.text = tr(&"hud_shell.kill_cam.return")
	else:
		_title.text = &"hud_shell.kill_cam.title"
		var lines := PackedStringArray()
		if not by.is_empty():
			lines.append(by)
		lines.append(tr(&"hud_shell.kill_cam.offer").format([duration]))
		_body.text = "\n".join(lines)
		_watch.text = tr(&"hud_shell.kill_cam.watch_again" if _played else &"hud_shell.kill_cam.watch")
	_dismiss.text = tr(&"hud_shell.kill_cam.dismiss")
	_progress.visible = _playing
	_dismiss.visible = true
