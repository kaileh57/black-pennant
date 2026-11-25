class_name AutoChip
extends Button
## "A" chip for an automatic value; hand glyph once overridden, with revert.
## DESIGN.md L351. Unit: ui-widgets.
##
## Sits beside a value an officer or the rules set (L351). While `overridden` is false it shows the
## A chip and is only a mark: it takes no focus or clicks. Once the player overrides the value it
## shows the hand glyph; pressing it fires `revert_requested` (one-click revert, rule 8), which the
## owner turns into `Orders.revert(id)` or the editor's equivalent. The owner then clears
## `overridden` when the data says the value is automatic again.
##
## - `default_text`: the automatic value as text ("040", "12 kn"); the tooltip names it.
## - `revertible` false: the hand marks a held control (hold-to-aim, rule 9) and nothing reverts.
## Icon-only, so it has an accessible name and a tooltip naming its effect (L1724 item 11).

signal revert_requested

var overridden := false:
	set(v):
		if is_same(v, overridden):
			return
		overridden = v
		_restyle()
var revertible := true:
	set(v):
		if is_same(v, revertible):
			return
		revertible = v
		_restyle()
var default_text := "":
	set(v):
		if is_same(v, default_text):
			return
		default_text = v
		_restyle()


func _ready() -> void:
	theme_type_variation = &"FlatButton"
	icon_alignment = HORIZONTAL_ALIGNMENT_CENTER
	expand_icon = false
	custom_minimum_size = Vector2(24, 24).max(custom_minimum_size)
	pressed.connect(_on_pressed)
	_restyle()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_restyle()


func _restyle() -> void:
	if not is_node_ready():
		return
	var acts := overridden and revertible
	icon = Ui.icon(&"hand" if overridden else &"chip_auto")
	focus_mode = Control.FOCUS_ALL if acts else Control.FOCUS_NONE
	mouse_filter = Control.MOUSE_FILTER_STOP if acts else Control.MOUSE_FILTER_PASS
	mouse_default_cursor_shape = Control.CURSOR_POINTING_HAND if acts else Control.CURSOR_ARROW
	var tip: String
	if not overridden:
		tip = tr(&"widgets.auto_chip.auto_tip")
		accessibility_name = tr(&"widgets.auto_chip.auto")
	elif revertible:
		tip = tr(&"widgets.auto_chip.revert_tip")
		accessibility_name = tr(&"widgets.auto_chip.revert")
	else:
		tip = tr(&"widgets.auto_chip.held_tip")
		accessibility_name = tr(&"widgets.auto_chip.held")
	if not default_text.is_empty() and overridden and revertible:
		tip += "\n" + tr(&"widgets.auto_chip.default").format([default_text])
	tooltip_text = tip


func _on_pressed() -> void:
	if overridden and revertible:
		revert_requested.emit()
