class_name Toast
extends Control
## Toast for global messages only ("Design saved", connection loss).
## DESIGN.md L1605, L1616. Unit: ui-widgets.
##
## `Screens` keeps one toast above every screen; call `Screens.toast(message)` rather than adding
## another. A toast never reports a sim event: sortie events go inline, to the log or to the alert
## strip (L1605). Only saves, joins or leaves and connection loss use it (L1616 toast check).
## - `show_message(message)`: a text key or `MsgDict`. It appears at once (no entrance motion,
##   L1603), stays `HOLD_S` seconds and disappears. A new message replaces the one showing and
##   restarts the hold; the same message again only restarts it.
## - `dismiss()` hides it now.
## The panel is a polite live region, so screen readers announce it. It never takes focus or clicks.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

## Seconds a message stays up. Long enough to read 80 characters at a glance.
const HOLD_S := 4.0
## Gap from the screen bottom, and above a control the message must not cover.
const BOTTOM_GAP := 48.0
const CLEAR_GAP := 12.0

@onready var _panel: PanelContainer = $Panel
@onready var _text: Label = $Panel/Text

var _message: Variant = &""
var _timer: SceneTreeTimer


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	_text.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_panel.visible = false


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready() and _panel.visible:
		_render()


func show_message(message: Variant) -> void:
	if _Text.is_empty(message):
		return
	_message = message
	_render()
	_panel.visible = true
	_timer = get_tree().create_timer(HOLD_S, true, false, true)
	_timer.timeout.connect(_on_hold_over.bind(_timer))


func dismiss() -> void:
	_timer = null
	_panel.visible = false


## True while a message is showing.
func is_showing() -> bool:
	return _panel.visible


func _render() -> void:
	var text := _Text.resolve(_message)
	_text.text = text
	_panel.accessibility_name = text
	# Keep the panel centred on its bottom anchor however long the text is.
	_panel.reset_size()
	var w := _panel.get_combined_minimum_size().x
	_panel.offset_left = -w * 0.5
	_panel.offset_right = w * 0.5
	_place_above_clear_zones()


## Lift the panel above any visible control in the `toast_clear` group (the sortie HUD's bottom
## row, where the helm sits), so a message never prints over live controls.
func _place_above_clear_zones() -> void:
	var h := _panel.get_combined_minimum_size().y
	var bottom := size.y - BOTTOM_GAP
	for n: Node in get_tree().get_nodes_in_group(&"toast_clear"):
		var c := n as Control
		if c != null and c.is_visible_in_tree():
			bottom = minf(bottom, c.get_global_rect().position.y - get_global_rect().position.y - CLEAR_GAP)
	_panel.offset_top = bottom - h - size.y
	_panel.offset_bottom = bottom - size.y


func _on_hold_over(timer: SceneTreeTimer) -> void:
	if timer == _timer:
		dismiss()
