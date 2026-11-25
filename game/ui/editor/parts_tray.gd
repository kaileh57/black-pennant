extends VBoxContainer
## Parts and kits tray.
## DESIGN.md L136-151, L244. Unit: ui-editor-tray.
## Reads and calls: Editor.tray, Editor.pick_part, Editor.pick_kit, Content.categories.
##
## What the dock may place, kits first (a newcomer drops four kits in the first ten minutes, L257), then
## single parts by category. Search matches names and descriptions; the category, tier and kind filters
## narrow the list. Each row shows its name, how many are placed against the ruleset limit, the campaign
## parts store count (L803: removed parts refit for free) and a lock with the reason when this dock may not
## place it (L1700: never colour alone).
##
## A click on a row (or Enter on a focused one) picks it: the ghost follows the cursor in the view, and a
## click there places it until Esc (L244). A row can also be dragged into the view (drag-to-place, L233).
## The spec sheet under the list shows the hovered or focused row (so a keyboard player reads it too), and
## the same sheet is the row's tooltip (L1700: first line 12 words or fewer, effect with units).
##
## The list follows the design: placed counts and availability update in place on `design_changed`; the
## rows are rebuilt only when the filter changes what is listed.

signal picked(id: StringName, is_kit: bool)
signal focused_item(item: Dictionary)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const ROW_H := 48.0
## Below this height the spec sheet would squeeze the list to nothing.
const SHORT_TRAY_H := 640.0
const ICON_PX := 24.0
## Lines of body text the spec sheet keeps room for.
const SPEC_LINES := 9.5
## OptionButton ids: -1 means "use the index", so "all" needs an id of its own.
const ALL := 9999
const KIND_ALL := 0
const KIND_KITS := 1
const KIND_PARTS := 2
## Placement words of `PartDefDict.placement`.
const PLACEMENTS: Array[StringName] = [&"anywhere", &"weather", &"keel", &"internal", &"deck_edge"]


## One tray row: a button that drags as a placement and shows the spec sheet as its tooltip.
class TrayRow extends Button:
	var item: Dictionary = {}
	var tray: Node
	var name_label: Label
	var detail: Label
	var lock: TextureRect
	var icon_rect: TextureRect

	func _get_drag_data(_at: Vector2) -> Variant:
		if not bool(item.get("available", true)) or disabled:
			return null
		var preview := Label.new()
		preview.theme_type_variation = &"Body"
		preview.text = name_label.text
		set_drag_preview(preview)
		return {"tray_id": StringName(item.get("id", &"")), "is_kit": StringName(item.get("kind", &"part")) == &"kit"}

	func _make_custom_tooltip(_for_text: String) -> Object:
		if tray == null:
			return null
		var panel := tray.call(&"spec_sheet", item, true) as Control
		return panel


@onready var _search: LineEdit = $Search
@onready var _category: OptionButton = $Filters/Category
@onready var _tier: OptionButton = $Filters2/Tier
@onready var _kind: OptionButton = $Filters2/Kind
@onready var _scroll: ScrollContainer = $Scroll
@onready var _list: VBoxContainer = $Scroll/List
@onready var _empty: VBoxContainer = $Empty
@onready var _empty_text: Label = $Empty/Text
@onready var _clear: Button = $Empty/Clear
@onready var _spec: VBoxContainer = $Spec
@onready var _rule: HSeparator = $Rule

var _rows: Dictionary[String, TrayRow] = {}
var _signature := ""
var _reload_pending := false
var _spec_item: Dictionary = {}
## The tallest spec sheet shown at this tray width: the sheet keeps that height, so hovering down the list
## never resizes it and moves the row under the pointer.
var _spec_floor := 0.0
var _spec_width := 0.0
var _picked_id := ""
var _spec_hidden := false
## The tray is too short for the list and the spec sheet together (a short window, UI scale 200%): the list
## keeps the room and the sheet shows only as the row's tooltip.
var _short := false
var _stale := false


func _ready() -> void:
	resized.connect(_on_resized)
	Ui.set_separation(self, 1)
	Ui.set_separation($Filters as BoxContainer, 1)
	Ui.set_separation($Filters2 as BoxContainer, 1)
	Ui.set_separation(_list, 0)
	Ui.set_separation(_spec, 1)
	_search.right_icon = Ui.icon(&"search")
	_search.accessibility_name = tr(&"editor_tray.search.name")
	_search.text_changed.connect(func(_t: String) -> void: _reload(true))
	_search.set_meta(&"control_id", "editor.tray.search")
	_category.item_selected.connect(func(_i: int) -> void: _reload(true))
	_tier.item_selected.connect(func(_i: int) -> void: _reload(true))
	_kind.item_selected.connect(func(_i: int) -> void: _reload(true))
	for f: Control in [_category, _tier, _kind]:
		f.set_meta(&"control_id", "editor.tray.filter")
	_clear.pressed.connect(_clear_filters)
	_clear.set_meta(&"control_id", "editor.tray.filter")
	accessibility_name = tr(&"editor_tray.name")
	_fill_filters()
	var editor := Kit.autoload(&"Editor")
	if editor != null:
		editor.connect(&"design_changed", func(_rev: int) -> void: _queue_reload())
		editor.connect(&"ghost_changed", _on_ghost_changed)
		editor.connect(&"selection_changed", _on_selection_changed)
	visibility_changed.connect(func() -> void:
		if _stale and is_visible_in_tree():
			_stale = false
			_reload(false))
	_reload(true)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_fill_filters()
		_reload(true)


func _fill_filters() -> void:
	var cat_keep := _category.get_selected_id()
	_category.clear()
	_category.add_item(tr(&"editor_tray.filter.all_categories"), ALL)
	for c: Dictionary in Kit.dicts(Kit.call_on(&"Content", &"categories")):
		var label := tr(StringName(c.get("label", &"")))
		_category.add_item(label, Kit.get_int(c, "category"))
		var stem := StringName(c.get("icon", &""))
		if stem != &"" and ResourceLoader.exists("res://ui/icons/%s.svg" % stem):
			_category.set_item_icon(_category.item_count - 1, Ui.icon(stem))
	_select_id(_category, cat_keep)
	_category.tooltip_text = tr(&"editor_tray.filter.category_tip")
	var tier_keep := _tier.get_selected_id()
	_tier.clear()
	_tier.add_item(tr(&"editor_tray.filter.all_tiers"), ALL)
	for t in 5:
		_tier.add_item(_word(&"tier", t), t)
	_select_id(_tier, tier_keep)
	_tier.tooltip_text = tr(&"editor_tray.filter.tier_tip")
	var kind_keep := _kind.get_selected_id()
	_kind.clear()
	_kind.add_item(tr(&"editor_tray.filter.kind_all"), KIND_ALL)
	_kind.add_item(tr(&"editor_tray.filter.kind_kits"), KIND_KITS)
	_kind.add_item(tr(&"editor_tray.filter.kind_parts"), KIND_PARTS)
	_select_id(_kind, kind_keep)
	_kind.tooltip_text = tr(&"editor_tray.filter.kind_tip")


static func _select_id(o: OptionButton, id: int) -> void:
	var i := o.get_item_index(id)
	o.select(i if i >= 0 else 0)


static func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null:
		return String(tx.call(&"word", kind, value))
	return ""


func _filter() -> Dictionary:
	var f := {}
	var text := _search.text.strip_edges()
	if not text.is_empty():
		f["search"] = text
	if _category.get_selected_id() != ALL:
		f["category"] = _category.get_selected_id()
	if _tier.get_selected_id() != ALL:
		f["tier"] = _tier.get_selected_id()
	match _kind.get_selected_id():
		KIND_KITS:
			f["kits_only"] = true
		KIND_PARTS:
			f["parts_only"] = true
	# A category names parts; kits are listed only without one.
	if f.has("category"):
		f["parts_only"] = true
	return f


func _filtered() -> bool:
	return not _search.text.strip_edges().is_empty() or _category.get_selected_id() != ALL or _tier.get_selected_id() != ALL or _kind.get_selected_id() != KIND_ALL


func _clear_filters() -> void:
	_search.text = ""
	_category.select(0)
	_tier.select(0)
	_kind.select(0)
	_reload(true)
	_search.grab_focus()


## Placed counts follow the design at most four times a second: a drag edits every frame, and reading
## the tray costs milliseconds on a big hull.
func _queue_reload() -> void:
	if _reload_pending:
		return
	if not is_visible_in_tree():
		_stale = true
		return
	_reload_pending = true
	await get_tree().create_timer(float(Ui.token("motion_ms.ceiling", 250)) / 1000.0).timeout
	if is_inside_tree():
		_reload(false)
	else:
		_reload_pending = false


## Read the tray. `rebuild` true after a filter change; otherwise rows are updated in place unless the
## listed set changed.
func _reload(rebuild: bool) -> void:
	_reload_pending = false
	if not is_node_ready():
		return
	var editor := Kit.autoload(&"Editor")
	var open := editor != null and bool(editor.call(&"is_open"))
	var items: Array[Dictionary] = []
	if open:
		items = Kit.dicts(editor.call(&"tray", _filter()))
	var ids: PackedStringArray = []
	for item: Dictionary in items:
		ids.append(String(item.get("kind", &"")) + ":" + String(item.get("id", &"")))
	var signature := ",".join(ids)
	if rebuild or signature != _signature:
		_signature = signature
		_rebuild(items)
	else:
		for item: Dictionary in items:
			var key := String(item.get("kind", &"")) + ":" + String(item.get("id", &""))
			if _rows.has(key):
				_update_row(_rows[key], item)
	if not open:
		_empty_text.text = tr(&"editor_tray.empty.closed")
		_clear.visible = false
	elif items.is_empty():
		_empty_text.text = tr(&"editor_tray.empty.filtered") if _filtered() else tr(&"editor_tray.empty.none")
		_clear.visible = _filtered()
	_empty.visible = items.is_empty()
	_scroll.visible = not items.is_empty()
	var spec_key := String(_spec_item.get("kind", &"")) + ":" + String(_spec_item.get("id", &""))
	if not _spec_item.is_empty() and _rows.has(spec_key):
		_show_spec(_rows[spec_key].item)
	else:
		_show_spec(_first_row_item())
	apply_tutorial()


## The item of the first row in list order (kits first).
func _first_row_item() -> Dictionary:
	for c: Node in _list.get_children():
		if c is TrayRow:
			return (c as TrayRow).item
	return {}


func _rebuild(items: Array[Dictionary]) -> void:
	for c: Node in _list.get_children():
		_list.remove_child(c)
		c.queue_free()
	_rows.clear()
	var section := ""
	var last_category := -2
	var focusables: Array[Control] = [_search]
	# Kits first: they come wired and ready to fight, and a newcomer starts with them (L257).
	var ordered: Array[Dictionary] = []
	for item: Dictionary in items:
		if StringName(item.get("kind", &"part")) == &"kit":
			ordered.append(item)
	for item: Dictionary in items:
		if StringName(item.get("kind", &"part")) != &"kit":
			ordered.append(item)
	for item: Dictionary in ordered:
		var is_kit := StringName(item.get("kind", &"part")) == &"kit"
		var want := "kits" if is_kit else "parts"
		if want != section:
			section = want
			last_category = -2
			_list.add_child(_section_title(&"editor_tray.section.kits" if is_kit else &"editor_tray.section.parts"))
		if not is_kit and _category.get_selected_id() == ALL:
			var c := Kit.get_int(item, "category", -1)
			if c != last_category:
				last_category = c
				var sub := Label.new()
				sub.theme_type_variation = &"Dense"
				sub.text = _word(&"category", c)
				sub.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
				_list.add_child(sub)
		var row := _make_row(item)
		_list.add_child(row)
		_rows[String(item.get("kind", &"")) + ":" + String(item.get("id", &""))] = row
		focusables.append(row)
	Kit.chain_vertical(focusables)


func _section_title(key: StringName) -> Label:
	var l := Label.new()
	l.theme_type_variation = &"PanelTitle"
	l.text = String(key)
	return l


func _make_row(item: Dictionary) -> TrayRow:
	var row := TrayRow.new()
	row.tray = self
	row.custom_minimum_size = Vector2(0.0, ROW_H)
	row.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.focus_mode = Control.FOCUS_ALL
	row.toggle_mode = false
	row.tooltip_text = " "
	var box := HBoxContainer.new()
	box.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT, Control.PRESET_MODE_MINSIZE, Ui.spacing(1))
	box.mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_separation(box, 1)
	row.add_child(box)
	row.icon_rect = TextureRect.new()
	row.icon_rect.custom_minimum_size = Vector2(ICON_PX, ICON_PX)
	row.icon_rect.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	row.icon_rect.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	row.icon_rect.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.icon_rect.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(row.icon_rect)
	row.name_label = Label.new()
	row.name_label.theme_type_variation = &"Body"
	row.name_label.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.name_label.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	row.name_label.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	row.name_label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(row.name_label)
	row.detail = Label.new()
	row.detail.theme_type_variation = &"Dense"
	row.detail.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	row.detail.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(row.detail)
	row.lock = TextureRect.new()
	row.lock.texture = Ui.icon(&"lock")
	row.lock.custom_minimum_size = Vector2(ICON_PX, ICON_PX)
	row.lock.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	row.lock.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	row.lock.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.lock.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(row.lock)
	row.pressed.connect(_on_row_pressed.bind(row))
	row.mouse_entered.connect(_show_spec_of.bind(row))
	row.focus_entered.connect(_show_spec_of.bind(row))
	_update_row(row, item)
	return row


func _update_row(row: TrayRow, item: Dictionary) -> void:
	row.item = item
	var is_kit := StringName(item.get("kind", &"part")) == &"kit"
	var id := String(item.get("id", ""))
	row.set_meta(&"control_id", "editor.tray." + id)
	row.name_label.text = item_name(item)
	var stem := StringName(item.get("icon", &""))
	if stem == &"" or not ResourceLoader.exists("res://ui/icons/%s.svg" % stem):
		stem = &"part_hull"
	row.icon_rect.texture = Ui.icon(stem)
	row.icon_rect.self_modulate = Ui.colour(&"text.primary")
	var available := Kit.get_bool(item, "available", true)
	row.lock.visible = not available
	row.lock.self_modulate = Ui.colour(&"text.secondary")
	row.detail.text = _detail_text(item)
	var name_text := row.name_label.text
	var acc := name_text
	if not row.detail.text.is_empty():
		acc = tr(&"editor_tray.row.accessible").format([name_text, row.detail.text])
	if not available:
		acc = tr(&"editor_tray.row.locked").format([name_text, _Text.resolve(item.get("locked_reason", {}))])
	row.accessibility_name = acc
	row.set_meta(&"is_kit", is_kit)
	row.set_meta(&"available", available)


## "2 of 4" against a ruleset limit, "2 placed" without one, plus the parts store count in a campaign.
func _detail_text(item: Dictionary) -> String:
	var parts: PackedStringArray = []
	var placed := Kit.get_int(item, "placed")
	var limit := Kit.get_int(item, "limit")
	if limit > 0:
		parts.append(tr(&"editor_tray.row.of_limit").format([Kit.number(float(placed), &"none", 0), Kit.number(float(limit), &"none", 0)]))
	elif placed > 0:
		parts.append(tr(&"editor_tray.row.placed").format([Kit.number(float(placed), &"none", 0)]))
	var store := Kit.get_int(item, "in_store")
	if store > 0:
		parts.append(tr(&"editor_tray.row.in_store").format([Kit.number(float(store), &"none", 0)]))
	return ", ".join(parts)


## The item's name in the player's language. A name key with no text row (a data gap) falls back to the id
## in words ("kit.example.gun76" reads "Example gun76") rather than showing a raw key.
static func item_name(item: Dictionary) -> String:
	var key := StringName(item.get("name", &""))
	var text := String(TranslationServer.translate(key))
	if text != String(key) or not String(key).contains("."):
		return text
	var id := String(item.get("id", key)).trim_prefix("kit.").trim_prefix("part.")
	var words := id.replace(".", " ").replace("_", " ").to_lower()
	return words.left(1).to_upper() + words.substr(1)


func _on_row_pressed(row: TrayRow) -> void:
	_show_spec(row.item)
	if not _allowed(String(row.get_meta(&"control_id", "editor.tray"))):
		return
	if not Kit.get_bool(row.item, "available", true):
		return
	pick(StringName(row.item.get("id", &"")), StringName(row.item.get("kind", &"part")) == &"kit")


## Start placing a tray item: it becomes the ghost (L244). False when this dock may not place it.
func pick(id: StringName, is_kit: bool) -> bool:
	var editor := Kit.autoload(&"Editor")
	if editor == null or id == &"":
		return false
	if not _allowed("editor.tray." + String(id)):
		return false
	var ok := bool(editor.call(&"pick_kit" if is_kit else &"pick_part", id))
	if ok:
		_picked_id = String(id)
		picked.emit(id, is_kit)
	return ok


## The inspector under the tray takes the room while parts are selected; the spec sheet steps aside.
func _on_selection_changed() -> void:
	var editor := Kit.autoload(&"Editor")
	var parts: Variant = Kit.as_dict(editor.call(&"selection")).get("parts", PackedInt32Array()) if editor != null else null
	var busy := parts is PackedInt32Array and not (parts as PackedInt32Array).is_empty()
	_spec_hidden = busy
	_spec.visible = not busy and not _short and _spec.get_child_count() > 0
	_rule.visible = _spec.visible


func _on_resized() -> void:
	if not is_equal_approx(size.x, _spec_width):
		_spec_width = size.x
		# Start from room for a typical sheet (a name, three lines of description, three rows of values
		# and a two-line hint), so the first hover down the list does not resize it either.
		var font := _spec.get_theme_font(&"font", &"Label")
		_spec_floor = font.get_height(_spec.get_theme_font_size(&"font_size", &"Label")) * SPEC_LINES if font != null else 0.0
		_spec.custom_minimum_size.y = _spec_floor
	var short := size.y > 0.0 and size.y < SHORT_TRAY_H
	if short == _short:
		return
	_short = short
	_spec.visible = not _spec_hidden and not _short and _spec.get_child_count() > 0
	_rule.visible = _spec.visible


func _on_ghost_changed() -> void:
	var editor := Kit.autoload(&"Editor")
	if editor != null and not editor.call(&"ghost") is Dictionary:
		_picked_id = ""


func _show_spec_of(row: TrayRow) -> void:
	_show_spec(row.item)


func _show_spec(item: Dictionary) -> void:
	_spec_item = item
	for c: Node in _spec.get_children():
		_spec.remove_child(c)
		c.queue_free()
	if item.is_empty():
		_spec.visible = false
		_rule.visible = false
		return
	_spec.visible = not _spec_hidden and not _short
	_rule.visible = _spec.visible
	_spec.add_child(spec_sheet(item, false))
	_hold_spec_height()
	focused_item.emit(item)


func _hold_spec_height() -> void:
	# Wrapped text knows its height once laid out at the tray's width.
	await get_tree().process_frame
	await get_tree().process_frame
	if not is_inside_tree() or _spec.get_child_count() == 0:
		return
	var h := (_spec.get_child(0) as Control).size.y
	if h > _spec_floor:
		_spec_floor = h
		_spec.custom_minimum_size.y = h


## The spec sheet of a tray item (L1700): name, effect, then values with units, placement, and the lock
## reason when it may not be placed. `tooltip` keeps it compact for the hover tooltip.
func spec_sheet(item: Dictionary, tooltip: bool) -> Control:
	var box := VBoxContainer.new()
	Ui.set_separation(box, 1)
	var is_kit := StringName(item.get("kind", &"part")) == &"kit"
	var title := Label.new()
	title.theme_type_variation = &"Body" if tooltip else &"PanelTitle"
	title.text = item_name(item)
	title.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	box.add_child(title)
	var desc_key := StringName(item.get("description", &""))
	if desc_key != &"":
		var desc := Label.new()
		desc.theme_type_variation = &"Dense"
		desc.text = tr(desc_key)
		desc.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		desc.custom_minimum_size = Vector2(360.0, 0.0)
		desc.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		box.add_child(desc)
	var grid := GridContainer.new()
	grid.columns = 2 if tooltip else 4
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	var rows: Array[Array] = []
	rows.append([&"editor_tray.spec.tier", _word(&"tier", Kit.get_int(item, "tier"))])
	if not is_kit:
		rows.append([&"editor_tray.spec.category", _word(&"category", Kit.get_int(item, "category"))])
	rows.append([&"editor_tray.spec.mass", Kit.number(Kit.get_float(item, "mass_kg") / 1000.0, &"t", 2)])
	rows.append([&"editor_tray.spec.cost", Kit.number(float(Kit.get_int(item, "cost")), &"none", 0)])
	var size: Variant = item.get("size", Vector3i.ONE)
	if size is Vector3i:
		var s := size as Vector3i
		rows.append([&"editor_tray.spec.size", tr(&"editor_tray.spec.size_value").format([Kit.number(float(s.x), &"none", 0), Kit.number(float(s.y), &"none", 0), Kit.number(float(s.z), &"none", 0)])])
	if not is_kit:
		var kw := Kit.get_float(item, "power_kw")
		if kw > 0.0:
			rows.append([&"editor_tray.spec.power_draw", Kit.number(kw, &"kw", 0)])
		elif kw < 0.0:
			rows.append([&"editor_tray.spec.power_supply", Kit.number(-kw, &"kw", 0)])
		var placement := StringName(item.get("placement", &"anywhere"))
		if placement in PLACEMENTS:
			rows.append([&"editor_tray.spec.placement", tr(StringName("editor_tray.placement." + String(placement)))])
		var stats: Variant = item.get("stats", [])
		if stats is Array:
			for st: Variant in stats as Array:
				if st is Dictionary:
					var d := st as Dictionary
					var unit := StringName(d.get("unit", &"none"))
					var v := float(d.get("value", 0.0))
					var decimals := 0 if is_equal_approx(v, roundf(v)) or absf(v) >= 10.0 or unit == &"kw" else 1
					rows.append([StringName(d.get("label", &"")), Kit.number(v, unit, decimals)])
	else:
		var kit_parts: Variant = item.get("parts", [])
		if kit_parts is Array:
			rows.append([&"editor_tray.spec.parts", Kit.number(float((kit_parts as Array).size()), &"none", 0)])
		rows.append([&"editor_tray.spec.version", Kit.number(float(Kit.get_int(item, "version", 1)), &"none", 0)])
		var author := Kit.get_str(item, "author")
		if not author.is_empty() and not Kit.get_bool(item, "stock", true):
			rows.append([&"editor_tray.spec.author", author])
	var limit := Kit.get_int(item, "limit")
	if limit > 0:
		rows.append([&"editor_tray.spec.limit", Kit.number(float(limit), &"none", 0)])
	var store := Kit.get_int(item, "in_store")
	if store > 0:
		rows.append([&"editor_tray.spec.store", Kit.number(float(store), &"none", 0)])
	for r: Array in rows:
		var k := Label.new()
		k.theme_type_variation = &"Dense"
		k.text = tr(r[0])
		k.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		grid.add_child(k)
		var v := Label.new()
		v.theme_type_variation = &"Dense"
		v.text = String(r[1])
		v.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		grid.add_child(v)
	box.add_child(grid)
	if not Kit.get_bool(item, "available", true):
		var row := HBoxContainer.new()
		Ui.set_separation(row, 1)
		var lock := TextureRect.new()
		lock.texture = Ui.icon(&"lock")
		lock.custom_minimum_size = Vector2(ICON_PX, ICON_PX)
		lock.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
		lock.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
		lock.size_flags_vertical = Control.SIZE_SHRINK_BEGIN
		row.add_child(lock)
		var why := Label.new()
		why.theme_type_variation = &"Dense"
		why.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		why.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		why.custom_minimum_size = Vector2(320.0, 0.0)
		var reason := _Text.resolve(item.get("locked_reason", {}))
		why.text = reason if not reason.is_empty() else tr(&"editor_tray.spec.locked")
		why.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		row.add_child(why)
		box.add_child(row)
	elif not tooltip:
		var how := Label.new()
		how.theme_type_variation = &"Dense"
		how.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
		how.text = tr(&"editor_tray.spec.how")
		how.custom_minimum_size = Vector2(320.0, 0.0)
		how.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		box.add_child(how)
	return box


func _allowed(id: String) -> bool:
	var t := Kit.autoload(&"Tutorial")
	return t == null or not t.has_method(&"is_allowed") or bool(t.call(&"is_allowed", id))


## Disable the rows and filters the current tutorial step does not allow (`editor.tray.<id>`).
func apply_tutorial() -> void:
	if not is_node_ready():
		return
	for key: String in _rows:
		var row := _rows[key]
		row.disabled = not _allowed(String(row.get_meta(&"control_id", "editor.tray")))
	_search.editable = _allowed("editor.tray.search")
	for f: BaseButton in [_category, _tier, _kind, _clear]:
		f.disabled = not _allowed("editor.tray.filter")
