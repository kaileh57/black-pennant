class_name MsgLabel
extends Label
## Label that shows a MsgDict through Tx.msg and re-translates on locale change.
## DESIGN.md L1707. Unit: ui-widgets.
##
## Set `msg` to a `MsgDict` (or a text key, or player-typed `String`); the label formats it with
## `Tx.msg` and formats it again when the locale changes, so no caller keeps English. Choose the
## look with `theme_type_variation` (Body by default). `hide_when_empty` hides the label while it
## has nothing to say, so an empty reason never leaves a gap.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

var msg: Variant = {}:
	set(v):
		if typeof(v) == typeof(msg) and v == msg:
			return
		msg = v
		_render()
var hide_when_empty := false:
	set(v):
		hide_when_empty = v
		_render()


func _ready() -> void:
	auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	if theme_type_variation == &"":
		theme_type_variation = &"Body"
	_render()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render()


## Show this message (same as setting `msg`).
func show_msg(message: Variant) -> void:
	msg = message


func _render() -> void:
	if not is_node_ready():
		return
	text = _Text.resolve(msg)
	if hide_when_empty:
		visible = not text.is_empty()
