class_name LegendRow
extends HBoxContainer
## Legend row: pattern or symbol and its label.
## DESIGN.md L1662, L1689. Unit: ui-widgets.
##
## One key entry: a 24 px swatch and its label (Dense). Legends explain uncertainty shapes (L1662),
## damage patterns (L1651) and the categorical editor and chart layers (L1689), so a colour or
## pattern is always named. Fill it with one of:
## - `show_icon(stem, label, tint)`: an icon of the set (`&"track_bearing_wedge"`,
##   `&"fill_hatch_45"`, `&"line_dashed"`) tinted with a palette token (`&"text.primary"`,
##   `&"alert.warning"`, `&"chart.overlay"`).
## - `show_stance(stance, domain, label)`: a stance symbol.
## - `show_damage(level, flood, label)`: a framed damage hatch sample.
## `label` is a text key or MsgDict. The tint follows palette changes.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")
const _Stance := preload("res://ui/widgets/stance_symbol.gd")
const _Hatch := preload("res://ui/widgets/damage_hatch.gd")

const SWATCH_PX := 24.0

var _swatch_holder: Control
var _text: Label
var _label_value: Variant = &""
var _tint: StringName = &"text.primary"
var _icon_rect: TextureRect


func _ready() -> void:
	_ensure_built()
	Ui.changed.connect(_on_ui_changed)


## Children are made on first need, so a row may be filled before it enters the tree.
func _ensure_built() -> void:
	if _text != null:
		return
	Ui.set_separation(self, 1)
	_swatch_holder = Control.new()
	_swatch_holder.name = &"Swatch"
	_swatch_holder.custom_minimum_size = Vector2(SWATCH_PX, SWATCH_PX)
	_swatch_holder.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	_swatch_holder.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(_swatch_holder)
	_text = Label.new()
	_text.name = &"Text"
	_text.theme_type_variation = &"Dense"
	_text.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_text.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	add_child(_text)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and _text != null:
		_render_label()


func show_icon(stem: StringName, label: Variant, tint: StringName = &"text.primary") -> void:
	_clear_swatch()
	_tint = tint
	_icon_rect = TextureRect.new()
	_icon_rect.texture = Ui.icon(stem)
	_icon_rect.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	_icon_rect.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	_icon_rect.set_anchors_preset(Control.PRESET_FULL_RECT)
	_icon_rect.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_icon_rect.self_modulate = Ui.colour(tint)
	_swatch_holder.add_child(_icon_rect)
	_set_label(label)


func show_stance(stance: int, domain: int, label: Variant) -> void:
	_clear_swatch()
	var symbol := _Stance.new()
	symbol.set_anchors_preset(Control.PRESET_FULL_RECT)
	symbol.symbol_px = SWATCH_PX
	symbol.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_swatch_holder.add_child(symbol)
	symbol.stance = stance
	symbol.domain = domain
	_set_label(label)


func show_damage(level: int, flood: float, label: Variant) -> void:
	_clear_swatch()
	var hatch := _Hatch.new()
	hatch.set_anchors_preset(Control.PRESET_FULL_RECT)
	hatch.framed = true
	_swatch_holder.add_child(hatch)
	hatch.level = level
	hatch.flood = flood
	_set_label(label)


func _clear_swatch() -> void:
	_ensure_built()
	_icon_rect = null
	for child: Node in _swatch_holder.get_children():
		child.queue_free()


func _set_label(label: Variant) -> void:
	_label_value = label
	_render_label()


func _render_label() -> void:
	_text.text = _Text.resolve(_label_value)


func _on_ui_changed(what: StringName) -> void:
	if what == &"palette" and _icon_rect != null and is_instance_valid(_icon_rect):
		_icon_rect.self_modulate = Ui.colour(_tint)
