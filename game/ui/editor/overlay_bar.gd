extends PanelContainer
## Overlay toggles and their legends.
## DESIGN.md L168, L285. Unit: ui-editor-shell.
## Reads and calls: BpEditorView.set_overlays, BpEditorView.legend.
##
## One toggle per overlay L168 and L285 name (waterline and heel, centres of gravity and buoyancy, weapon
## arcs with blocked sectors hatched, sensor blind sectors, conduit routes, X-ray of the three networks,
## vulnerability heatmap, compartments, signature). Under them, while any overlay is on, the legend names
## every colour and pattern the view draws (L1689: categorical overlays always carry a legend and a line
## pattern or label), in the categorical overlay palette the view reads from tokens.json.
##
## The dry dock owns the view: it hands it over with `bind_view` and listens to `overlays_changed`. The
## choice of overlays lasts for the session (static), so a splash and back keeps it. Waterline starts on:
## Float is the first gauge a newcomer fixes (L254).

signal overlays_changed(bits: int)

const Compiler := preload("res://ui/theme/theme_compiler.gd")
const Kit := preload("res://ui/screens/menu_kit.gd")
const SWATCH_PX := 24.0

## Toggle order, text key, tooltip key. The bits are `Bp.OVERLAY_*`.
const TOGGLES: Array[Array] = [
	[1, &"editor_shell.overlay.waterline", &"editor_shell.overlay.waterline_tip"],
	[2, &"editor_shell.overlay.centres", &"editor_shell.overlay.centres_tip"],
	[4, &"editor_shell.overlay.arcs", &"editor_shell.overlay.arcs_tip"],
	[8, &"editor_shell.overlay.blind", &"editor_shell.overlay.blind_tip"],
	[16, &"editor_shell.overlay.conduits", &"editor_shell.overlay.conduits_tip"],
	[64, &"editor_shell.overlay.xray", &"editor_shell.overlay.xray_tip"],
	[32, &"editor_shell.overlay.heatmap", &"editor_shell.overlay.heatmap_tip"],
	[128, &"editor_shell.overlay.compartments", &"editor_shell.overlay.compartments_tip"],
	[256, &"editor_shell.overlay.signature", &"editor_shell.overlay.signature_tip"],
]

## The view's pattern names to icons of the set.
const PATTERN_ICONS: Dictionary[StringName, StringName] = {
	&"line_solid": &"line_solid", &"line_dashed": &"line_dashed", &"line_dotted": &"line_dotted",
	&"cross": &"close", &"ring": &"shape_circle", &"fill": &"fill_solid", &"hatch": &"fill_hatch_45",
	&"badge": &"line_double", &"heat_low": &"fill_solid", &"heat_high": &"fill_solid", &"outline": &"shape_rect",
}

## Legend text key to the colour the view draws it in: an `overlay` palette key, or a `ui:` token.
const ROW_COLOURS: Dictionary[StringName, String] = {
	&"dock.overlay.waterline": "waterline", &"dock.overlay.design_waterline": "waterline",
	&"dock.overlay.cg": "cg", &"dock.overlay.cb": "cb", &"dock.overlay.arc": "arc",
	&"dock.overlay.arc_blocked": "ui:blocked", &"dock.overlay.blind": "blind",
	&"dock.overlay.power": "power", &"dock.overlay.fuel": "fuel", &"dock.overlay.data": "data",
	&"dock.overlay.doubled": "ui:text.primary", &"dock.overlay.heat_low": "heat_low", &"dock.overlay.heat_high": "heat_high",
	&"dock.overlay.planes": "plane", &"dock.overlay.rcs": "signature_rcs", &"dock.overlay.visual": "signature_visual",
	&"dock.overlay.ghost": "ghost", &"dock.overlay.ghost_invalid": "ui:alert.warning",
	&"dock.overlay.ghost_preview": "ghost_preview", &"dock.overlay.ghost_removed": "ui:alert.warning",
}

static var session_bits: int = 1

@onready var _toggles: HFlowContainer = $Margin/Column/Toggles
@onready var _legend: HFlowContainer = $Margin/Column/Legend
@onready var _menu: MenuButton = $Margin/Column/Menu

var _view: Node
var _buttons: Dictionary[int, Button] = {}
var _legend_pending := false


func _ready() -> void:
	Ui.set_margins($Margin as MarginContainer, 1)
	Ui.set_separation($Margin/Column as BoxContainer, 1)
	_toggles.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	_toggles.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	_legend.add_theme_constant_override(&"h_separation", Ui.spacing(4))
	_legend.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	accessibility_name = tr(&"editor_shell.overlay.name")
	_build_toggles()
	_menu.get_popup().id_pressed.connect(func(bit: int) -> void: toggle(bit))
	_menu.get_popup().hide_on_checkable_item_selection = false
	var editor := Kit.autoload(&"Editor")
	if editor != null and editor.has_signal(&"ghost_changed"):
		editor.connect(&"ghost_changed", _queue_legend)
	if Ui.has_signal(&"changed"):
		Ui.changed.connect(_on_ui_changed)
	_rebuild_legend()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_build_toggles()
		_rebuild_legend()


## Give the bar the view it switches; the session's overlays go on at once.
func bind_view(view: Node) -> void:
	_view = view
	_apply()


## Narrow screens: one Overlays menu with a check item per overlay instead of the row of toggles.
func set_compact(on: bool) -> void:
	_toggles.visible = not on
	_menu.visible = on


func bits() -> int:
	return session_bits


## Flip one overlay (the X key flips X-ray).
func toggle(bit: int) -> void:
	_set_bits(session_bits ^ bit)


## Turn overlays on without turning any off (the inspector's "Show arcs").
func ensure(on: int) -> void:
	_set_bits(session_bits | on)


func _set_bits(value: int) -> void:
	if value == session_bits:
		return
	session_bits = value
	for bit: int in _buttons:
		_buttons[bit].set_pressed_no_signal(session_bits & bit != 0)
	_sync_menu()
	_apply()


func _apply() -> void:
	if _view != null and is_instance_valid(_view):
		_view.call(&"set_overlays", session_bits)
	overlays_changed.emit(session_bits)
	_queue_legend()


func _build_toggles() -> void:
	for c: Node in _toggles.get_children():
		_toggles.remove_child(c)
		c.queue_free()
	_buttons.clear()
	for row: Array in TOGGLES:
		var bit: int = row[0]
		var b := Button.new()
		b.toggle_mode = true
		b.text = String(row[1])
		b.tooltip_text = tr(row[2])
		b.custom_minimum_size = Vector2(0.0, 40.0)
		b.focus_mode = Control.FOCUS_ALL
		b.set_pressed_no_signal(session_bits & bit != 0)
		b.set_meta(&"control_id", "editor.overlay." + String(row[1]).get_slice(".", 2))
		b.toggled.connect(_on_toggled.bind(bit))
		_toggles.add_child(b)
		_buttons[bit] = b
	_sync_menu()
	var list: Array = _buttons.values()
	for i in list.size():
		var b: Button = list[i]
		b.focus_neighbor_left = b.get_path_to(list[i - 1] if i > 0 else b)
		b.focus_neighbor_right = b.get_path_to(list[i + 1] if i < list.size() - 1 else b)


func _sync_menu() -> void:
	var popup := _menu.get_popup()
	popup.clear()
	for row: Array in TOGGLES:
		var bit: int = row[0]
		popup.add_check_item(tr(row[1]), bit)
		var i := popup.get_item_index(bit)
		popup.set_item_checked(i, session_bits & bit != 0)
		popup.set_item_tooltip(i, tr(row[2]))
	_menu.tooltip_text = tr(&"editor_shell.overlay.menu_tip")


func _on_toggled(on: bool, bit: int) -> void:
	_set_bits(session_bits | bit if on else session_bits & ~bit)


func _on_ui_changed(what: StringName) -> void:
	if what == &"palette":
		_rebuild_legend()


## The view swaps its ghost legend once it has drawn the new ghost, a frame or more after the change, so
## the legend is read on the next frame and again a moment later.
func _queue_legend() -> void:
	if _legend_pending or not is_inside_tree():
		return
	_legend_pending = true
	await get_tree().process_frame
	if is_inside_tree():
		_rebuild_legend()
	await get_tree().create_timer(float(Ui.token("motion_ms.ceiling", 250)) / 1000.0).timeout
	_legend_pending = false
	if is_inside_tree():
		_rebuild_legend()


func _rebuild_legend() -> void:
	if not is_node_ready():
		return
	for c: Node in _legend.get_children():
		_legend.remove_child(c)
		c.queue_free()
	var rows: Array = []
	if _view != null and is_instance_valid(_view):
		var got: Variant = _view.call(&"legend")
		if got is Array:
			rows = got
	for r: Variant in rows:
		if not r is Dictionary:
			continue
		var row := r as Dictionary
		var text := StringName(row.get("text", &""))
		var pattern := StringName(row.get("pattern", &"fill"))
		_legend.add_child(_legend_row(PATTERN_ICONS.get(pattern, &"fill_solid"), text, _row_colour(text)))
	_legend.visible = _legend.get_child_count() > 0


func _legend_row(icon: StringName, text: StringName, tint: Color) -> HBoxContainer:
	var box := HBoxContainer.new()
	Ui.set_separation(box, 1)
	box.mouse_filter = Control.MOUSE_FILTER_IGNORE
	var swatch := TextureRect.new()
	swatch.texture = Ui.icon(icon)
	swatch.custom_minimum_size = Vector2(SWATCH_PX, SWATCH_PX)
	swatch.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	swatch.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	swatch.self_modulate = tint
	swatch.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	swatch.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(swatch)
	var label := Label.new()
	label.theme_type_variation = &"Dense"
	label.text = String(text)
	label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(label)
	return box


## The colour a legend row's mark is drawn in, the same token the view reads for it.
func _row_colour(text: StringName) -> Color:
	var key: String = ROW_COLOURS.get(text, "ui:text.primary")
	if key == "ui:blocked":
		key = "ui:text.primary" if Ui.palette == &"high_contrast" else "ui:alert.warning"
	if key.begins_with("ui:"):
		return Ui.colour(StringName(key.trim_prefix("ui:")))
	var overlay: Variant = Ui.token("palettes.overlay")
	if overlay is Dictionary and (overlay as Dictionary).has(key):
		return Compiler.hex(String((overlay as Dictionary)[key]))
	return Ui.colour(&"text.primary")
