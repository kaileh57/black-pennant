class_name ModalDialog
extends Control
## Modal dialog with effect-verb buttons.
## DESIGN.md L1697. Unit: ui-widgets.
##
## Open dialogs through `Screens.open_dialog(title, body, buttons, cancel_id, focus_id)`, which
## stacks them above every screen, restores focus when they close and frees them. Used alone:
## instance res://ui/widgets/dialog.tscn, add it, call `configure`, and listen to `chosen`.
##
## - `title`, `body`: a text key (`StringName`), a `MsgDict` (`Dictionary`) or a plain `String`
##   shown as it is (player-typed names only). An empty body hides the body line.
## - `buttons`: `Array` of `{ "id": StringName, "label": StringName }`, shown left to right. Labels
##   are effect verbs ("Delete design", "Keep editing"), never "OK" (L1697 copy rule 3).
## - `cancel_id`: the choice Esc makes; `&""` means Esc does nothing (a decision the player must
##   make). `focus_id`: the button that takes focus; for a destructive question give the safe one.
## The dialog takes every input event while open, so nothing below it acts.

## Loaded by path so this script parses even where the global class cache is stale.
const _Text := preload("res://ui/widgets/ui_text.gd")

signal chosen(id: StringName)

const MIN_BUTTON_WIDTH := 160.0

@onready var _scrim: ColorRect = $Scrim
@onready var _panel: PanelContainer = $Center/Panel
@onready var _title: Label = $Center/Panel/Margin/Column/Title
@onready var _body: Label = $Center/Panel/Margin/Column/Body
@onready var _buttons_row: HBoxContainer = $Center/Panel/Margin/Column/Buttons

var _cancel_id: StringName = &""
var _focus_id: StringName = &""
var _buttons: Array[Button] = []
var _done := false
var _title_value: Variant = &""
var _body_value: Variant = &""
var _spec: Array = []


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_STOP
	Ui.set_margins($Center/Panel/Margin, 2)
	Ui.set_separation($Center/Panel/Margin/Column, 3)
	Ui.set_separation(_buttons_row, 2)
	_title.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_body.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_apply_scrim()
	Ui.changed.connect(_on_ui_changed)
	_title.visible = false
	_body.visible = false


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render_text()


## Fill the dialog. See the file header for the argument shapes.
func configure(title: Variant, body: Variant, buttons: Array, cancel_id: StringName = &"", focus_id: StringName = &"") -> void:
	if not is_node_ready():
		await ready
	_title_value = title
	_body_value = body
	_spec = buttons.duplicate()
	_cancel_id = cancel_id
	_focus_id = focus_id
	for b: Button in _buttons:
		b.queue_free()
	_buttons.clear()
	for entry: Variant in _spec:
		if not entry is Dictionary:
			continue
		var d := entry as Dictionary
		var b := Button.new()
		b.name = StringName("Choice_" + String(d.get("id", "")))
		b.custom_minimum_size = Vector2(MIN_BUTTON_WIDTH, 0.0)
		b.focus_mode = Control.FOCUS_ALL
		b.pressed.connect(choose.bind(StringName(d.get("id", &""))))
		b.set_meta(&"label_key", d.get("label", &""))
		_buttons_row.add_child(b)
		_buttons.append(b)
	_link_focus()
	_render_text()
	Ui.focus(default_focus())


## Close with this choice. Fires `chosen` once.
func choose(id: StringName) -> void:
	if _done:
		return
	_done = true
	chosen.emit(id)


## The button named by `focus_id`, else the cancel button, else the first.
func default_focus() -> Control:
	for wanted: StringName in [_focus_id, _cancel_id]:
		if wanted == &"":
			continue
		for b: Button in _buttons:
			if b.name == StringName("Choice_" + String(wanted)):
				return b
	return _buttons[0] if not _buttons.is_empty() else null


func _unhandled_input(event: InputEvent) -> void:
	if _done:
		return
	if event.is_action_pressed(&"ui_cancel", false, true) and _cancel_id != &"":
		choose(_cancel_id)
	elif get_viewport().gui_get_focus_owner() == null and _is_direction(event):
		Ui.focus(default_focus())
	get_viewport().set_input_as_handled()


static func _is_direction(event: InputEvent) -> bool:
	for action: StringName in [&"ui_left", &"ui_right", &"ui_up", &"ui_down"]:
		if event.is_action_pressed(action, false, true):
			return true
	return false


func _link_focus() -> void:
	var n := _buttons.size()
	for i in n:
		var b := _buttons[i]
		var left := _buttons[(i - 1 + n) % n]
		var right := _buttons[(i + 1) % n]
		b.focus_neighbor_left = b.get_path_to(left)
		b.focus_neighbor_right = b.get_path_to(right)
		b.focus_previous = b.get_path_to(left)
		b.focus_next = b.get_path_to(right)
		b.focus_neighbor_top = b.get_path_to(b)
		b.focus_neighbor_bottom = b.get_path_to(b)


func _render_text() -> void:
	var title_text := _Text.resolve(_title_value)
	var body_text := _Text.resolve(_body_value)
	_title.text = title_text
	_title.visible = not title_text.is_empty()
	_body.text = body_text
	_body.visible = not body_text.is_empty()
	_panel.accessibility_name = title_text
	_panel.accessibility_description = body_text
	for b: Button in _buttons:
		b.text = tr(StringName(b.get_meta(&"label_key", &"")))


func _apply_scrim() -> void:
	_scrim.color = Ui.scrim_colour()


func _on_ui_changed(what: StringName) -> void:
	if what == &"transparency" or what == &"palette":
		_apply_scrim()
