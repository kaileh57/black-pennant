extends VBoxContainer
## Armour tiers, palette with clamped picker, markings.
## DESIGN.md L128-130, L1801-1803. Unit: ui-editor-hull.
## Reads and calls: Editor.palette, Editor.set_palette_entry, Editor.paint_armour, Editor.paint_colour, Editor.add_marking, BpEditorView.brush_cells.
##
## Three modes, each a dry-dock tool; the dock turns strokes in the view into the Editor calls, reading the
## brush, tier, palette entry and marking settings from here:
## - Armour (`Bp.TOOL_ARMOUR`, L128): none, 5, 15, 40 or 80 mm on outer cells, with what the last stroke
##   added in mass and cost.
## - Colour (`Bp.TOOL_PAINT`, L130, L1803): the ship palette of 16 entries. The eight US Navy swatches and
##   the antifouling colour are fixed; the seven custom entries take a colour from the picker, which shows
##   the colour actually stored after the clamp (hull Y 0.6 and OKLCH chroma 0.06, markings Y 0.7 and 0.12,
##   chroma first) and says when the pick was clamped. A picking session is one undo step; Cancel puts the
##   entry back.
## - Markings (`Bp.TOOL_MARKING`, L130): stripe, flag or hull number in a palette entry and a size; a click
##   on a hull face places it (mirrored to the other side on a mirrored hull), and the list removes them.
## Brush size is a radius in cells (0 paints one cell).

signal tool_requested(tool: int)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const FIELD_H := 40.0
const SWATCH_PX := 48.0
const MAX_BRUSH := 8
## Palette entries 0..8 are the naval swatches and antifouling (`PaintDict.locked`).
const FIRST_CUSTOM := 9
const PALETTE_SIZE := 16
## Default colour brush: hull.haze, the usual side colour.
const DEFAULT_PAINT := 2
## Armour tiers in `Bp.ARMOUR_*` order and their thickness in millimetres (L128).
const ARMOUR_MM: Array[int] = [0, 5, 15, 40, 80]
const MARKING_KINDS: Array[StringName] = [&"editor_hull.marking.stripe", &"editor_hull.marking.flag", &"editor_hull.marking.hull_number"]
const MAX_MARKING_TEXT := 16

enum Page { ARMOUR, COLOUR, MARKINGS }


## A palette entry: a square in the entry's colour; the selected one carries the 3 px selection frame.
class Swatch extends Button:
	var entry: Dictionary = {}
	var selected := false
	var fill: Color

	func _draw() -> void:
		var r := Rect2(Vector2(4.0, 4.0), size - Vector2(8.0, 8.0))
		if bool(entry.get("empty", false)):
			draw_rect(r, get_theme_color(&"rule_divider", &"Plot"), false, 1.0)
			var plus := Ui.icon(&"add")
			if plus != null:
				draw_texture_rect(plus, Rect2(r.get_center() - Vector2(12.0, 12.0), Vector2(24.0, 24.0)), false, get_theme_color(&"text_secondary", &"Plot"))
		else:
			draw_rect(r, fill)
			draw_rect(r, get_theme_color(&"rule_divider", &"Plot"), false, 1.0)
		if bool(entry.get("locked", false)):
			var lock := Ui.icon(&"lock")
			if lock != null:
				draw_texture_rect(lock, Rect2(r.end - Vector2(18.0, 18.0), Vector2(16.0, 16.0)), false, get_theme_color(&"text_primary", &"Plot"))
		if selected:
			draw_rect(Rect2(Vector2(1.5, 1.5), size - Vector2(3.0, 3.0)), get_theme_color(&"select", &"Plot"), false, 3.0)


@onready var _mode_buttons: Array[Button] = [$Modes/Armour, $Modes/Colour, $Modes/Markings]
@onready var _page: VBoxContainer = $Scroll/Page
@onready var _picker: PopupPanel = $Picker
@onready var _picker_title: Label = $Picker/Margin/Column/Title
@onready var _picker_use: OptionButton = $Picker/Margin/Column/Use
@onready var _picker_colour: ColorPicker = $Picker/Margin/Column/Colour
@onready var _picker_stored: Control = $Picker/Margin/Column/Result/Stored
@onready var _picker_note: Label = $Picker/Margin/Column/Result/Note
@onready var _picker_apply: Button = $Picker/Margin/Column/Buttons/Apply
@onready var _picker_cancel: Button = $Picker/Margin/Column/Buttons/Cancel
@onready var _debounce: Timer = $Debounce

var _current_page: int = Page.ARMOUR
var _tier := 1
var _brush := 1
var _paint_index := DEFAULT_PAINT
var _marking_kind := 2
var _marking_paint := 7
var _marking_size := 2
var _marking_text := ""
var _palette: Array[Dictionary] = []
var _swatches: Array[Swatch] = []
var _layout := ""
var _refresh_pending := false
var _last_stroke: Dictionary = {}
var _stroke_line: Label
var _entry_line: Label
var _edit_button: Button
var _markings_list: VBoxContainer
var _marking_paint_pick: OptionButton
# The picking session: the entry index being edited, whether a group is open, the colour shown stored.
var _picking := -1
var _picking_open := false
var _picking_cancelled := false
var _stored_colour: Color
var _stale := false


func _ready() -> void:
	Ui.set_separation(self, 2)
	Ui.set_separation($Modes as BoxContainer, 1)
	Ui.set_separation(_page, 2)
	accessibility_name = tr(&"editor_hull.paint.name")
	var group := ButtonGroup.new()
	var ids: Array[String] = ["editor.paint.armour", "editor.paint.colour", "editor.paint.marking"]
	var tips: Array[StringName] = [&"editor_hull.paint.mode.armour_tip", &"editor_hull.paint.mode.colour_tip", &"editor_hull.paint.mode.markings_tip"]
	for i in _mode_buttons.size():
		_mode_buttons[i].button_group = group
		_mode_buttons[i].pressed.connect(_on_mode.bind(i))
		_mode_buttons[i].set_meta(&"control_id", ids[i])
		_mode_buttons[i].tooltip_text = tr(tips[i])
	for m: MarginContainer in [$Picker/Margin]:
		Ui.set_margins(m, 3)
	Ui.set_separation($Picker/Margin/Column as BoxContainer, 2)
	Ui.set_separation($Picker/Margin/Column/Result as BoxContainer, 2)
	Ui.set_separation($Picker/Margin/Column/Buttons as BoxContainer, 2)
	_picker_use.add_item(tr(&"editor_hull.picker.use_hull"), 0)
	_picker_use.add_item(tr(&"editor_hull.picker.use_marking"), 1)
	_picker_use.item_selected.connect(func(_i: int) -> void: _store_pick())
	_picker_colour.color_changed.connect(func(_c: Color) -> void: _debounce.start())
	_debounce.timeout.connect(_store_pick)
	_picker_stored.draw.connect(_draw_stored)
	_picker_apply.pressed.connect(_close_picker.bind(false))
	_picker_cancel.pressed.connect(_close_picker.bind(true))
	_picker.popup_hide.connect(_on_picker_hidden)
	var editor := _editor()
	if editor != null:
		editor.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
		editor.connect(&"tool_changed", _on_tool_changed)
	if Ui.has_signal(&"changed"):
		Ui.changed.connect(func(what: StringName) -> void:
			if what == &"palette":
				for s: Swatch in _swatches:
					s.queue_redraw())
	visibility_changed.connect(func() -> void:
		if _stale and is_visible_in_tree():
			_stale = false
			_refresh())
	_refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_layout = ""
		_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


# --- what the dock asks -----------------------------------------------------------------------------

func tool() -> int:
	match _current_page:
		Page.COLOUR:
			return Bp.TOOL_PAINT
		Page.MARKINGS:
			return Bp.TOOL_MARKING
	return Bp.TOOL_ARMOUR


func brush_radius() -> int:
	return _brush


func armour_tier() -> int:
	return _tier


func paint_index() -> int:
	return _paint_index


## `{ "kind": Bp.MARKING_*, "paint": int, "size_cells": int, "text": String }` for the next marking.
func marking_settings() -> Dictionary:
	return {"kind": _marking_kind, "paint": _marking_paint, "size_cells": _marking_size, "text": _marking_text}


## What the stroke under way (or just ended) changed: `{ "tool", "cells", "mass_delta_kg", "cost_delta" }`.
func show_stroke(total: Dictionary) -> void:
	_last_stroke = total
	_show_stroke_line()


# --- modes ------------------------------------------------------------------------------------------

func _on_mode(page: int) -> void:
	if not _allowed(String(_mode_buttons[page].get_meta(&"control_id"))):
		_mode_buttons[_current_page].set_pressed_no_signal(true)
		return
	_current_page = page
	_layout = ""
	_refresh()
	tool_requested.emit(tool())


func _on_tool_changed(t: int) -> void:
	var want := -1
	match t:
		Bp.TOOL_ARMOUR:
			want = Page.ARMOUR
		Bp.TOOL_PAINT:
			want = Page.COLOUR
		Bp.TOOL_MARKING:
			want = Page.MARKINGS
	if want >= 0 and want != _current_page:
		_current_page = want
		_layout = ""
		_refresh()


# --- refresh ----------------------------------------------------------------------------------------

func _queue_refresh() -> void:
	if _refresh_pending:
		return
	if not is_visible_in_tree():
		_stale = true
		return
	_refresh_pending = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_pending = false
	if not is_node_ready():
		return
	for i in _mode_buttons.size():
		_mode_buttons[i].set_pressed_no_signal(i == _current_page)
	var e := _editor()
	var open := e != null and bool(e.call(&"is_open"))
	var markings: Array[Dictionary] = []
	_palette = []
	if open:
		_palette = Kit.dicts(e.call(&"palette"))
		markings = Kit.dicts(e.call(&"markings"))
	var layout := "%s:%d:%d" % [str(open), _current_page, markings.size() if _current_page == Page.MARKINGS else 0]
	if layout != _layout:
		_layout = layout
		_build(open, markings)
	_update_values(markings)
	apply_tutorial()


func _build(open: bool, markings: Array[Dictionary]) -> void:
	for c: Node in _page.get_children():
		_page.remove_child(c)
		c.queue_free()
	_swatches.clear()
	_stroke_line = null
	_entry_line = null
	_edit_button = null
	_markings_list = null
	_marking_paint_pick = null
	if not open:
		_page.add_child(Kit.label("editor_hull.closed", &"Body"))
		return
	match _current_page:
		Page.ARMOUR:
			_build_armour()
		Page.COLOUR:
			_build_colour()
		Page.MARKINGS:
			_build_markings(markings)


func _brush_row() -> void:
	var row := Kit.hbox(1)
	var l := Kit.label("editor_hull.paint.brush", &"Body")
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(l)
	var f := SpinBox.new()
	f.min_value = 0
	f.max_value = MAX_BRUSH
	f.step = 1
	f.rounded = true
	f.value = _brush
	f.custom_minimum_size = Vector2(160.0, FIELD_H)
	f.tooltip_text = tr(&"editor_hull.paint.brush_tip")
	f.get_line_edit().accessibility_name = tr(&"editor_hull.paint.brush")
	f.value_changed.connect(func(v: float) -> void: _brush = int(v))
	f.set_meta(&"control_id", "editor.paint.brush")
	row.add_child(f)
	_page.add_child(row)


func _build_armour() -> void:
	_page.add_child(Kit.label("editor_hull.paint.armour_title", &"PanelTitle"))
	_page.add_child(Kit.label("editor_hull.paint.armour_body", &"Dense"))
	var group := ButtonGroup.new()
	var tips: Array[StringName] = [&"editor_hull.armour.none_tip", &"editor_hull.armour.5_tip", &"editor_hull.armour.15_tip", &"editor_hull.armour.40_tip", &"editor_hull.armour.80_tip"]
	var focusables: Array[Control] = []
	for i in ARMOUR_MM.size():
		var b := Button.new()
		b.toggle_mode = true
		b.button_group = group
		b.alignment = HORIZONTAL_ALIGNMENT_LEFT
		b.custom_minimum_size = Vector2(0.0, FIELD_H)
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.text = tr(&"editor_hull.armour.none") if i == 0 else Kit.number(float(ARMOUR_MM[i]), &"mm", 0)
		b.tooltip_text = tr(tips[i])
		b.set_pressed_no_signal(i == _tier)
		b.pressed.connect(func() -> void: _tier = i)
		b.set_meta(&"control_id", "editor.paint.armour")
		_page.add_child(b)
		focusables.append(b)
	Kit.chain_vertical(focusables)
	_brush_row()
	_stroke_line = Kit.label("", &"Dense", true)
	_page.add_child(_stroke_line)
	_show_stroke_line()


func _build_colour() -> void:
	_page.add_child(Kit.label("editor_hull.paint.colour_title", &"PanelTitle"))
	_page.add_child(Kit.label("editor_hull.paint.colour_body", &"Dense"))
	var grid := GridContainer.new()
	grid.columns = 4
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	for i in PALETTE_SIZE:
		var s := Swatch.new()
		s.custom_minimum_size = Vector2(SWATCH_PX, SWATCH_PX)
		s.focus_mode = Control.FOCUS_ALL
		s.pressed.connect(_on_swatch.bind(i))
		s.set_meta(&"control_id", "editor.paint.colour")
		grid.add_child(s)
		_swatches.append(s)
	_page.add_child(grid)
	for i in _swatches.size():
		var s := _swatches[i]
		s.focus_neighbor_left = s.get_path_to(_swatches[i - 1] if i % 4 > 0 else s)
		s.focus_neighbor_right = s.get_path_to(_swatches[i + 1] if i % 4 < 3 and i + 1 < _swatches.size() else s)
		s.focus_neighbor_top = s.get_path_to(_swatches[i - 4] if i >= 4 else s)
		s.focus_neighbor_bottom = s.get_path_to(_swatches[i + 4] if i + 4 < _swatches.size() else s)
	_entry_line = Kit.label("", &"Body", true)
	_page.add_child(_entry_line)
	_edit_button = Kit.inline_button("editor_hull.paint.edit")
	_edit_button.custom_minimum_size.y = FIELD_H
	_edit_button.pressed.connect(_open_picker)
	_edit_button.set_meta(&"control_id", "editor.paint.colour")
	_page.add_child(_edit_button)
	_brush_row()
	_stroke_line = Kit.label("", &"Dense", true)
	_page.add_child(_stroke_line)
	_show_stroke_line()


func _build_markings(markings: Array[Dictionary]) -> void:
	_page.add_child(Kit.label("editor_hull.paint.markings_title", &"PanelTitle"))
	_page.add_child(Kit.label("editor_hull.paint.markings_body", &"Dense"))
	var kinds := Kit.hbox(1)
	var group := ButtonGroup.new()
	for i in MARKING_KINDS.size():
		var b := Kit.inline_button(String(MARKING_KINDS[i]))
		b.toggle_mode = true
		b.button_group = group
		b.custom_minimum_size.y = FIELD_H
		b.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		b.set_pressed_no_signal(i == _marking_kind)
		b.pressed.connect(func() -> void:
			_marking_kind = i
			_layout = ""
			_refresh())
		b.set_meta(&"control_id", "editor.paint.marking")
		kinds.add_child(b)
	_page.add_child(kinds)
	var grid := GridContainer.new()
	grid.columns = 2
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	grid.add_child(_grid_label("editor_hull.marking.paint"))
	_marking_paint_pick = OptionButton.new()
	_marking_paint_pick.custom_minimum_size = Vector2(220.0, FIELD_H)
	_marking_paint_pick.accessibility_name = tr(&"editor_hull.marking.paint")
	_marking_paint_pick.item_selected.connect(func(i: int) -> void: _marking_paint = _marking_paint_pick.get_item_id(i))
	_marking_paint_pick.set_meta(&"control_id", "editor.paint.marking")
	grid.add_child(_marking_paint_pick)
	grid.add_child(_grid_label("editor_hull.marking.size"))
	var size := SpinBox.new()
	size.min_value = 1
	size.max_value = 64
	size.step = 1
	size.rounded = true
	size.value = _marking_size
	size.suffix = tr(&"editor_hull.paint.cells")
	size.custom_minimum_size = Vector2(160.0, FIELD_H)
	size.tooltip_text = tr(&"editor_hull.marking.size_tip")
	size.get_line_edit().accessibility_name = tr(&"editor_hull.marking.size")
	size.value_changed.connect(func(v: float) -> void: _marking_size = int(v))
	size.set_meta(&"control_id", "editor.paint.marking")
	grid.add_child(size)
	if _marking_kind == Bp.MARKING_HULL_NUMBER:
		grid.add_child(_grid_label("editor_hull.marking.text"))
		var text := LineEdit.new()
		text.max_length = MAX_MARKING_TEXT
		text.text = _marking_text
		text.placeholder_text = tr(&"editor_hull.marking.text_placeholder")
		text.custom_minimum_size = Vector2(160.0, FIELD_H)
		text.accessibility_name = tr(&"editor_hull.marking.text")
		text.text_changed.connect(func(t: String) -> void: _marking_text = t.strip_edges())
		text.set_meta(&"control_id", "editor.paint.marking")
		grid.add_child(text)
	_page.add_child(grid)
	_page.add_child(HSeparator.new())
	_page.add_child(Kit.label("editor_hull.marking.placed", &"PanelTitle"))
	_markings_list = VBoxContainer.new()
	Ui.set_separation(_markings_list, 1)
	_page.add_child(_markings_list)
	_fill_markings(markings)


func _fill_markings(markings: Array[Dictionary]) -> void:
	if _markings_list == null:
		return
	for c: Node in _markings_list.get_children():
		_markings_list.remove_child(c)
		c.queue_free()
	if markings.is_empty():
		_markings_list.add_child(Kit.label("editor_hull.marking.none", &"Dense"))
		return
	for m: Dictionary in markings:
		var row := Kit.hbox(1)
		var kind := clampi(Kit.get_int(m, "kind"), 0, MARKING_KINDS.size() - 1)
		var text := tr(&"editor_hull.marking.row").format([tr(MARKING_KINDS[kind]), Kit.number(float(Kit.get_int(m, "size_cells")), &"none", 0), _face_word(Kit.get_int(m, "face"))])
		if kind == Bp.MARKING_HULL_NUMBER and not Kit.get_str(m, "text").is_empty():
			text = tr(&"editor_hull.marking.row_text").format([text, Kit.get_str(m, "text")])
		var l := Kit.label(text, &"Body", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		var index := Kit.get_int(m, "index")
		var remove := Kit.icon_button(&"delete", tr(&"editor_hull.marking.remove").format([text]))
		remove.custom_minimum_size = Vector2(FIELD_H, FIELD_H)
		remove.pressed.connect(func() -> void: _editor().call(&"remove_marking", index))
		remove.set_meta(&"control_id", "editor.paint.marking")
		row.add_child(remove)
		_markings_list.add_child(row)


## A field label for a two-column grid: one line, taking the spare width.
static func _grid_label(key: String) -> Label:
	var l := Kit.label(key, &"Body")
	l.autowrap_mode = TextServer.AUTOWRAP_OFF
	l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	return l


static func _face_word(face: int) -> String:
	var keys: Array[StringName] = [&"editor_hull.face.aft", &"editor_hull.face.forward", &"editor_hull.face.below", &"editor_hull.face.above", &"editor_hull.face.port", &"editor_hull.face.starboard"]
	return TranslationServer.translate(keys[clampi(face, 0, 5)])


func _update_values(markings: Array[Dictionary]) -> void:
	for i in _swatches.size():
		var s := _swatches[i]
		s.entry = _palette[i] if i < _palette.size() else {"empty": true, "index": i}
		s.fill = _display_colour(s.entry)
		s.selected = i == _paint_index
		var name := _entry_name(s.entry)
		s.tooltip_text = name
		s.accessibility_name = name
		s.queue_redraw()
	if _entry_line != null:
		var entry: Dictionary = _palette[_paint_index] if _paint_index < _palette.size() else {"empty": true, "index": _paint_index}
		var use := tr(&"editor_hull.paint.use_marking") if Kit.get_bool(entry, "marking") else tr(&"editor_hull.paint.use_hull")
		_entry_line.text = tr(&"editor_hull.paint.entry").format([_entry_name(entry), use]) if not Kit.get_bool(entry, "empty") else _entry_name(entry)
		_edit_button.visible = _paint_index >= FIRST_CUSTOM
		_edit_button.text = tr(&"editor_hull.paint.pick") if Kit.get_bool(entry, "empty") else tr(&"editor_hull.paint.edit")
	if _marking_paint_pick != null:
		_marking_paint_pick.clear()
		for i in _palette.size():
			var entry := _palette[i]
			if Kit.get_bool(entry, "empty"):
				continue
			_marking_paint_pick.add_item(_entry_name(entry), i)
		var at := _marking_paint_pick.get_item_index(_marking_paint)
		if at < 0 and _marking_paint_pick.item_count > 0:
			at = 0
			_marking_paint = _marking_paint_pick.get_item_id(0)
		if at >= 0:
			_marking_paint_pick.select(at)
	if _current_page == Page.MARKINGS:
		_fill_markings(markings)


## A palette entry's name: the naval swatch by its paint, antifouling, a custom colour by number.
func _entry_name(entry: Dictionary) -> String:
	var index := Kit.get_int(entry, "index")
	if Kit.get_bool(entry, "empty"):
		return tr(&"editor_hull.swatch.empty").format([Kit.number(float(index - FIRST_CUSTOM + 1), &"none", 0)])
	var token := String(entry.get("token", &""))
	if not token.is_empty():
		return tr(StringName("editor_hull.swatch." + token.replace(".", "_")))
	return tr(&"editor_hull.swatch.custom").format([Kit.number(float(index - FIRST_CUSTOM + 1), &"none", 0)])


## The entry's stored linear colour as a display colour (sRGB), built from a token colour so no colour is
## written as a literal.
static func _display_colour(entry: Dictionary) -> Color:
	var c := Ui.colour(&"surface.inset")
	var v: Variant = entry.get("rgb", Vector3.ZERO)
	if v is Vector3:
		c.r = clampf((v as Vector3).x, 0.0, 1.0)
		c.g = clampf((v as Vector3).y, 0.0, 1.0)
		c.b = clampf((v as Vector3).z, 0.0, 1.0)
		c.a = 1.0
		c = c.linear_to_srgb()
	return c


func _on_swatch(index: int) -> void:
	if not _allowed("editor.paint.colour"):
		return
	var entry: Dictionary = _palette[index] if index < _palette.size() else {"empty": true}
	_paint_index = index
	_update_values([])
	# An empty custom entry has nothing to paint with yet: pick its colour first.
	if Kit.get_bool(entry, "empty"):
		_open_picker()


func _show_stroke_line() -> void:
	if _stroke_line == null or not is_instance_valid(_stroke_line):
		return
	if _last_stroke.is_empty() or Kit.get_int(_last_stroke, "cells") <= 0:
		_stroke_line.text = ""
		return
	var cells := Kit.number(float(Kit.get_int(_last_stroke, "cells")), &"none", 0)
	if Kit.get_int(_last_stroke, "tool") == Bp.TOOL_ARMOUR:
		var mass := Kit.get_float(_last_stroke, "mass_delta_kg") / 1000.0
		var cost := Kit.get_int(_last_stroke, "cost_delta")
		_stroke_line.text = tr(&"editor_hull.paint.armour_stroke").format([cells, _signed(mass, &"t", 2), _signed(float(cost), &"none", 0)])
	else:
		_stroke_line.text = tr(&"editor_hull.paint.colour_stroke").format([cells])


static func _signed(v: float, unit: StringName, decimals: int) -> String:
	var s := Kit.number(absf(v), unit, decimals)
	if v > 0.0:
		return TranslationServer.translate(&"editor_hull.paint.plus").format([s])
	if v < 0.0:
		return TranslationServer.translate(&"editor_hull.paint.minus").format([s])
	return s


# --- the picker -------------------------------------------------------------------------------------

func _open_picker() -> void:
	if _paint_index < FIRST_CUSTOM or not _allowed("editor.paint.colour"):
		return
	var entry: Dictionary = _palette[_paint_index] if _paint_index < _palette.size() else {"empty": true}
	var e := _editor()
	if e == null:
		return
	_picking = _paint_index
	_picking_cancelled = false
	_picker_title.text = _entry_name(entry) if not Kit.get_bool(entry, "empty") else tr(&"editor_hull.picker.new")
	_picker_use.select(1 if Kit.get_bool(entry, "marking") else 0)
	var start := _display_colour(entry) if not Kit.get_bool(entry, "empty") else _display_colour(_palette[DEFAULT_PAINT] if DEFAULT_PAINT < _palette.size() else {})
	_picker_colour.color = start
	_stored_colour = start
	_picker_note.text = tr(&"editor_hull.picker.limits_hull") if _picker_use.selected == 0 else tr(&"editor_hull.picker.limits_marking")
	e.call(&"begin_group", &"dock.undo.palette")
	_picking_open = true
	_picker.popup_centered()
	_picker_apply.grab_focus.call_deferred()
	_picker_stored.queue_redraw()


## Store the pick now: the Editor clamps it and the swatch shows what was stored, so the player sees the
## clamp at once (L1803).
func _store_pick() -> void:
	if _picking < 0 or not _picking_open:
		return
	var e := _editor()
	if e == null:
		return
	var lin := _picker_colour.color.srgb_to_linear()
	var marking := _picker_use.selected == 1
	var stored := Kit.as_dict(e.call(&"set_palette_entry", _picking, Vector3(lin.r, lin.g, lin.b), marking))
	if stored.is_empty():
		_picker_note.text = tr(&"editor_hull.picker.refused")
		return
	_picking = Kit.get_int(stored, "index", _picking)
	_paint_index = _picking
	_stored_colour = _display_colour(stored)
	var picked := _picker_colour.color
	var clamped := absf(picked.r - _stored_colour.r) + absf(picked.g - _stored_colour.g) + absf(picked.b - _stored_colour.b) > 0.02
	if clamped:
		_picker_note.text = tr(&"editor_hull.picker.clamped_marking") if marking else tr(&"editor_hull.picker.clamped_hull")
	else:
		_picker_note.text = tr(&"editor_hull.picker.within_marking") if marking else tr(&"editor_hull.picker.within_hull")
	_picker_stored.queue_redraw()


func _draw_stored() -> void:
	var r := Rect2(Vector2.ZERO, _picker_stored.size)
	_picker_stored.draw_rect(r, _stored_colour)
	_picker_stored.draw_rect(r, get_theme_color(&"rule_divider", &"Plot"), false, 1.0)


func _close_picker(cancel: bool) -> void:
	_picking_cancelled = cancel
	if not cancel and not _debounce.is_stopped():
		_debounce.stop()
		_store_pick()
	_picker.hide()


## Closing the picker by any route ends its undo group: Cancel restores the entry, anything else keeps it.
func _on_picker_hidden() -> void:
	if not _picking_open:
		return
	if not _debounce.is_stopped() and not _picking_cancelled:
		_debounce.stop()
		_store_pick()
	_debounce.stop()
	_picking_open = false
	var e := _editor()
	if e != null:
		e.call(&"end_group", _picking_cancelled)
	_picking = -1
	_queue_refresh()


func _allowed(id: String) -> bool:
	var t := Kit.autoload(&"Tutorial")
	return t == null or not t.has_method(&"is_allowed") or bool(t.call(&"is_allowed", id))


func apply_tutorial() -> void:
	for c: Node in find_children("*", "Control", true, false):
		if not c.has_meta(&"control_id"):
			continue
		var ok := _allowed(String(c.get_meta(&"control_id")))
		if c is BaseButton:
			(c as BaseButton).disabled = not ok
		elif c is SpinBox:
			(c as SpinBox).editable = ok
		elif c is LineEdit:
			(c as LineEdit).editable = ok
