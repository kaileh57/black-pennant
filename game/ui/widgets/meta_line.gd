class_name MetaLine
extends Label
## Meta line bound to game data beside a heading (never above it).
## DESIGN.md L1578. Unit: ui-widgets.
##
## The one exception to the eyebrow ban (L1578): a line of data the heading lacks (a track or hull
## number, a date-time group, "Mission 3 of 7"). It passes Cover (hide it and the heading still
## reads) and Swap (move it elsewhere and it is wrong), uses the Meta look (body size, no tracking)
## and sits after or beside its heading. Each instance must be bound in
## res://ui/lint/meta_fields.txt to the data it shows, and a screen has at most ceil(panels / 4).
##
## `show_value(value)`: a `MsgDict`, a text key or a plain `String` of data (a hull number). The
## line hides itself while it has no data, so it never shows a placeholder.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

var value: Variant = "":
	set(v):
		if typeof(v) == typeof(value) and v == value:
			return
		value = v
		_render()


func _ready() -> void:
	auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	theme_type_variation = &"Meta"
	uppercase = false
	_render()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render()


func show_value(new_value: Variant) -> void:
	value = new_value


func _render() -> void:
	if not is_node_ready():
		return
	text = _Text.resolve(value)
	visible = not text.is_empty()
