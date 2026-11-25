extends Control
## Loading and shader precompile cover with progress; also the persistent safe-mode notice when the Compatibility renderer is in use.
## DESIGN.md L1833, L1270. Unit: ui-boot.
## Reads and calls: Session.loading_progress, App.startup_report, BpWorldView.warmed_up.
##
## `Screens` keeps one instance above every screen and drives it; other scripts use
## `Screens.show_loading`, `Screens.hide_loading` and `Screens.set_safe_mode`.
## - `cover(text, fraction)`: an opaque cover with a status line (text key or `MsgDict`) and a
##   progress bar. `fraction` 0..1, or below 0 when the amount of work is not known, which hides
##   the bar rather than showing a spinner (L1667 rule 8: no spinners). While covering, every input
##   event stops here, so nothing under the cover can be pressed.
## - `caption(text, fraction)`: the same status and bar in a strip at the bottom with the picture left
##   visible, for work the player should watch (the first-launch graphics check). Input stops here
##   too.
## - `follow_session(on)`: while covering, take progress from `Session.loading_progress`.
## - `cover_until_warmed(view, text)`: cover until a BpWorldView emits `warmed_up` (the shader
##   precompile screen, L1833). Returns at once when `view` is null.
## - `set_progress(fraction)`, `uncover()`, `is_covering()`.
## - `set_safe_mode(on)`: the persistent notice that the driver lacks Direct3D 12, Metal and Vulkan
##   support, that the player should update it and that the mode is unsupported (L1270). It stays
##   on every screen, under the cover too, and never takes clicks. `refresh_safe_mode()` reads it
##   from `App.startup_report()`.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

signal uncovered

@onready var _cover: Panel = $Cover
@onready var _status: Label = $Cover/Center/Column/Status
@onready var _bar: ProgressBar = $Cover/Center/Column/Bar
@onready var _notice: PanelContainer = $Notice
@onready var _strip: PanelContainer = $Strip
@onready var _strip_status: Label = $Strip/Column/Status
@onready var _strip_bar: ProgressBar = $Strip/Column/Bar

var _text: Variant = &""
var _following := false


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_separation($Cover/Center/Column, 3)
	Ui.set_separation($Strip/Column, 1)
	_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_strip_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_cover.visible = false
	_strip.visible = false
	_notice.visible = false
	var session := get_node_or_null(^"/root/Session")
	if session != null and session.has_signal(&"loading_progress"):
		session.connect(&"loading_progress", _on_session_progress)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready() and is_covering():
		_render()


func _input(event: InputEvent) -> void:
	if is_covering() and not (event is InputEventMouseMotion):
		get_viewport().set_input_as_handled()


func cover(text: Variant, fraction: float = -1.0) -> void:
	_show(text, fraction, false)


func caption(text: Variant, fraction: float = -1.0) -> void:
	_show(text, fraction, true)


func _show(text: Variant, fraction: float, see_through: bool) -> void:
	_text = text
	_following = false
	_cover.visible = not see_through
	_strip.visible = see_through
	get_viewport().gui_release_focus()
	_render()
	set_progress(fraction)


func set_progress(fraction: float) -> void:
	var known := is_finite(fraction) and fraction >= 0.0
	_bar.visible = known
	_strip_bar.visible = known
	if known:
		_bar.value = clampf(fraction, 0.0, 1.0)
		_strip_bar.value = _bar.value


func uncover() -> void:
	_following = false
	if not is_covering():
		return
	_cover.visible = false
	_strip.visible = false
	uncovered.emit()


func is_covering() -> bool:
	return _cover.visible or _strip.visible


func follow_session(on: bool) -> void:
	_following = on and is_covering()


## Cover until `view` (a BpWorldView) emits `warmed_up`, then uncover. Awaitable.
func cover_until_warmed(view: Node, text: Variant) -> void:
	if view == null or not view.has_signal(&"warmed_up"):
		return
	cover(text, -1.0)
	await Signal(view, &"warmed_up")
	uncover()


func set_safe_mode(on: bool) -> void:
	_notice.visible = on
	_notice.accessibility_name = tr(&"boot.safe_mode.notice") if on else ""


## Show the notice when the startup report says the game runs in safe mode.
func refresh_safe_mode() -> void:
	var app := get_node_or_null(^"/root/App")
	if app == null:
		return
	var report: Variant = app.call(&"startup_report")
	set_safe_mode(report is Dictionary and bool((report as Dictionary).get("safe_mode", false)))


func _render() -> void:
	var text := _Text.resolve(_text)
	_status.text = text
	_strip_status.text = text
	_bar.accessibility_name = text
	_strip_bar.accessibility_name = text


func _on_session_progress(fraction: float) -> void:
	if _following and is_covering():
		set_progress(fraction)
