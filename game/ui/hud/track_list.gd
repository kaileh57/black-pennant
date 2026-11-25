extends Control
## Docked track list, sorted by threat; accessible track table.
## DESIGN.md L1660-1663, L1723. Unit: ui-hud-tracks.
## Reads and calls: Hud.track_table, Hud.tracks_changed, Hud.select_track, Orders.designate.
##
## One Tree is both the docked list and the screen-reader track table (accessibility floor 12): a
## row per track the side holds, in threat order by default (inbound weapons, Hostile inside own
## weapon range by closest approach, Hostile outside, Suspect, Unknown rung, Neutral, Friend, then
## last-known marks). Columns: symbol (stance frame and domain modifier, tinted, rule 4), track
## number (tabular), stance word, ID rung as zero to four bars plus the word (rule 7), bearing and
## range from the own hull. Each row's description is the track's accessible name ("Track 0412,
## Hostile, bearing 047"), so a screen reader reads it as one item.
## - Sort: the Sort button (threat, range, bearing, number) or a column title (track: number,
##   bearing and range: range, the others: threat); the same title again reverses the order. The
##   choice holds for the sortie.
## - Click or arrow onto a row: select it (the track card opens). Double-click or Enter: designate
##   it for the officers. The designated track's row is bold and the count line names it.
## - Rows update in place at the 10 Hz track rate (`Hud.tracks_changed`); items are reused by
##   track number and only moved when the order changes, so selection, scroll and focus hold.
## - States: waiting for the first picture, no contacts, populated. Last-known marks are grey.
## The Tree's incremental search is off, so letter keys reach the sortie (H, F, Z...).

const Kit := preload("res://ui/screens/menu_kit.gd")

const SORTS: Array[StringName] = [&"threat", &"range", &"bearing", &"number"]
const SORT_KEYS: Array[StringName] = [&"hud_tracks.sort.threat", &"hud_tracks.sort.range", &"hud_tracks.sort.bearing", &"hud_tracks.sort.number"]
const COL_SYMBOL := 0
const COL_NUMBER := 1
const COL_ID := 2
const COL_POSITION := 3
const COLUMNS := 4
const COLUMN_TITLES: Array[StringName] = [&"", &"hud_tracks.column.track", &"hud_tracks.column.id", &"hud_tracks.column.position"]
## Fixed column widths at 100% UI scale (canvas units); track and ID share what is left.
const SYMBOL_W := 32
const NUMBER_MIN_W := 134
const ID_MIN_W := 112
const POSITION_W := 148
## Below this table width (a narrow dock at a large UI scale) the table drops to three columns:
## symbol, track number and position. The stance stays in the symbol's frame and tint, and the
## stance word and ID rung read in the row's tooltip, its accessible name and the track card
## (floor 2: abbreviation allowed at 200%).
const NARROW_W := 380.0
const NARROW_NUMBER_W := 64
const NARROW_POSITION_W := 140
const STANCE_NAMES: Array[String] = ["friend", "neutral", "suspect", "hostile"]
const DOMAIN_NAMES: Array[String] = ["surface", "subsurface", "air", "weapon"]
const STANCE_TOKENS: Array[StringName] = [&"stance.friend", &"stance.neutral", &"stance.suspect", &"stance.hostile"]

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column
@onready var _header: HFlowContainer = $Margin/Column/Header
@onready var _sort_button: OptionButton = $Margin/Column/Header/Sort
@onready var _counts: Label = $Margin/Column/Counts
@onready var _table: Tree = $Margin/Column/Table
@onready var _empty: Label = $Margin/Column/Empty

var _sort: StringName = &"threat"
var _descending := false
var _root: TreeItem
var _items: Dictionary[int, TreeItem] = {}
var _order: Array[int] = []
var _selected := -1
var _syncing := false
var _live := false
var _bold: Font
## Height the HUD shell gives the list (`set_max_height`); below it the table scrolls. -1: no cap.
var _max_height := -1.0
## Three-column table for a narrow dock.
var _narrow := false


func _ready() -> void:
	# 16 px from the dock's edges, as every docked cluster; the table's own frame takes 4 of them at
	# the sides, so the columns keep their room.
	Ui.set_margins(_margin, 3)
	_margin.add_theme_constant_override(&"margin_left", Ui.spacing(2))
	_margin.add_theme_constant_override(&"margin_right", Ui.spacing(2))
	Ui.set_separation(_column, 1)
	# A flow: in a narrow dock (large UI scale) the sort button moves under the title.
	_header.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	_header.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	_counts.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_table.allow_search = false
	_table.allow_reselect = true
	_table.select_mode = Tree.SELECT_ROW
	_table.focus_mode = Control.FOCUS_ALL
	_table.accessibility_name = tr(&"hud_tracks.access.table")
	_table.accessibility_description = tr(&"hud_tracks.access.table_description")
	_root = _table.create_item()
	_table.item_selected.connect(_on_item_selected)
	_table.item_activated.connect(_on_item_activated)
	_table.column_title_clicked.connect(_on_title_clicked)
	_table.nothing_selected.connect(_on_nothing_selected)
	_sort_button.focus_mode = Control.FOCUS_ALL
	_sort_button.tooltip_text = tr(&"hud_tracks.sort.tooltip")
	_sort_button.accessibility_name = tr(&"hud_tracks.sort.name")
	_fill_sort_items()
	_sort_button.item_selected.connect(_on_sort_selected)
	Kit.chain_vertical([_sort_button, _table])
	_setup_columns()
	resized.connect(_fit_columns)
	_column.minimum_size_changed.connect(_fit_height, CONNECT_DEFERRED)
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		_live = true
		if hud.has_signal(&"tracks_changed"):
			hud.connect(&"tracks_changed", refresh)
		if hud.has_signal(&"selection_changed"):
			hud.connect(&"selection_changed", _on_selection_changed)
		if hud.has_signal(&"ship_static_changed"):
			hud.connect(&"ship_static_changed", refresh)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: refresh())
	refresh()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_TRANSLATION_CHANGED:
			if is_node_ready():
				_fill_sort_items()
				_setup_columns()
				_rebuild()
		NOTIFICATION_THEME_CHANGED:
			if is_node_ready():
				_bold = null
				_rebuild()


## The control the HUD shell focuses when the player moves into the docks: the table.
func default_focus() -> Control:
	return _table


func _fill_sort_items() -> void:
	_sort_button.clear()
	for i in SORTS.size():
		_sort_button.add_item(tr(SORT_KEYS[i]), i)
	_sort_button.select(SORTS.find(_sort))


func _setup_columns() -> void:
	for c in COLUMN_TITLES.size():
		_table.set_column_title(c, tr(COLUMN_TITLES[c]) if COLUMN_TITLES[c] != &"" else "")
		_table.set_column_clip_content(c, true)
	_table.set_column_title_tooltip_text(COL_SYMBOL, tr(&"hud_tracks.column.symbol_tooltip"))
	_table.set_column_title_tooltip_text(COL_NUMBER, tr(&"hud_tracks.column.sort_tooltip"))
	_table.set_column_title_tooltip_text(COL_ID, tr(&"hud_tracks.column.sort_tooltip"))
	_table.set_column_title_tooltip_text(COL_POSITION, tr(&"hud_tracks.column.sort_tooltip"))
	_fit_columns()


func _fit_columns() -> void:
	if not is_node_ready():
		return
	var narrow := size.x > 1.0 and size.x < NARROW_W
	if narrow != _narrow:
		_narrow = narrow
		_rebuild.call_deferred()
	_table.set_column_title(COL_ID, "" if _narrow else tr(COLUMN_TITLES[COL_ID]))
	_table.set_column_expand(COL_SYMBOL, false)
	_table.set_column_custom_minimum_width(COL_SYMBOL, SYMBOL_W)
	# Below these widths the table scrolls sideways instead of cutting text (floor 2: one-axis
	# scroll at 200% UI scale).
	for c: int in [COL_NUMBER, COL_ID]:
		_table.set_column_expand(c, true)
	_table.set_column_custom_minimum_width(COL_NUMBER, NUMBER_MIN_W)
	_table.set_column_custom_minimum_width(COL_ID, ID_MIN_W)
	# Spare width splits evenly between the number and stance word and the rung.
	_table.set_column_expand_ratio(COL_NUMBER, 1)
	_table.set_column_expand_ratio(COL_ID, 1)
	_table.set_column_expand(COL_POSITION, false)
	_table.set_column_custom_minimum_width(COL_POSITION, POSITION_W)
	if _narrow:
		_table.set_column_expand(COL_ID, false)
		_table.set_column_custom_minimum_width(COL_ID, 0)
		_table.set_column_custom_minimum_width(COL_NUMBER, NARROW_NUMBER_W)
		_table.set_column_custom_minimum_width(COL_POSITION, NARROW_POSITION_W)


# --- data ----------------------------------------------------------------------------------------

## Pull the table from Hud and update the rows.
func refresh() -> void:
	if not is_node_ready():
		return
	var rows: Array[Dictionary] = []
	if _live:
		rows = Kit.dicts(Kit.call_on(&"Hud", &"track_table", [_sort, _descending]))
	var sel: Variant = Kit.call_on(&"Hud", &"selected_track")
	_selected = int(sel) if sel is int else -1
	show_rows(rows, _counts_dict(), _waiting())


## The viewer's own hull, which the side picture also holds as a track; it is not a contact.
func _own_vessel() -> int:
	var m := Kit.as_dict(Kit.call_on(&"Hud", &"motion"))
	if m.has("vessel"):
		return Kit.get_int(m, "vessel", Bp.NONE)
	return Kit.get_int(Kit.as_dict(Kit.call_on(&"Session", &"info")), "vessel", Bp.NONE)


func _counts_dict() -> Dictionary:
	return Kit.as_dict(Kit.call_on(&"Hud", &"track_counts")) if _live else {}


func _waiting() -> bool:
	if not _live:
		return true
	var session := Kit.autoload(&"Session")
	if session == null:
		return true
	var state := int(session.call(&"state"))
	if state == Bp.SESSION_LOADING or state == Bp.SESSION_IDLE:
		return true
	var rev: Variant = Kit.call_on(&"Hud", &"rev", [&"tracks"])
	return state == Bp.SESSION_IN_SORTIE and rev is int and int(rev) == 0 and Kit.call_on(&"Hud", &"has_ship") != true


## Draw `rows` (`TrackDict`s in display order) with the header `counts` (`Hud.track_counts`).
## `waiting` shows the before-the-first-picture state. Also the test entry point.
func show_rows(rows: Array[Dictionary], counts: Dictionary, waiting: bool = false) -> void:
	var ids: Array[int] = []
	var designated := ""
	var own := _own_vessel()
	var live := 0
	var tally: Dictionary[String, int] = {"hostile": 0, "suspect": 0, "inbound": 0}
	for t: Dictionary in rows:
		var id := Kit.get_int(t, "id", -1)
		if id <= 0 or ids.has(id):
			continue
		if own != Bp.NONE and Kit.get_int(t, "vessel", Bp.NONE) == own:
			continue
		if not Kit.get_bool(t, "last_known"):
			live += 1
			var st := Kit.get_int(t, "stance", -1)
			if st == Bp.STANCE_HOSTILE:
				tally["hostile"] += 1
			elif st == Bp.STANCE_SUSPECT:
				tally["suspect"] += 1
			if Kit.get_bool(t, "inbound"):
				tally["inbound"] += 1
		ids.append(id)
		var item: TreeItem = _items.get(id)
		if item == null:
			item = _table.create_item(_root)
			item.set_metadata(COL_SYMBOL, id)
			_items[id] = item
		_fill(item, t)
		if Kit.get_bool(t, "designated"):
			designated = Kit.get_str(t, "label")
	for id: int in _items.keys():
		if not ids.has(id):
			var gone: TreeItem = _items[id]
			_items.erase(id)
			_root.remove_child(gone)
			gone.free()
	if ids != _order:
		var prev: TreeItem = null
		for id: int in ids:
			var item: TreeItem = _items[id]
			if prev == null:
				if _root.get_first_child() != item:
					item.move_before(_root.get_first_child())
			elif prev.get_next() != item:
				item.move_after(prev)
			prev = item
		_order = ids
	_sync_selection()
	var total := live
	counts = counts.duplicate()
	for k: String in tally:
		counts[k] = tally[k]
	_table.visible = not ids.is_empty()
	_empty.visible = ids.is_empty()
	_empty.text = tr(&"hud_tracks.waiting" if waiting else &"hud_tracks.empty")
	_counts.visible = not ids.is_empty()
	_counts.text = _count_line(counts, total, designated)
	_fit_height.call_deferred()


## The list takes the height its rows need, up to `height` (the HUD shell shares each dock's
## height out); past it the table scrolls (floor 2).
func set_max_height(height: float) -> void:
	if is_equal_approx(height, _max_height):
		return
	_max_height = height
	_fit_height()


func _fit_height() -> void:
	if not is_node_ready():
		return
	var want := _natural_height()
	if _max_height >= 0.0:
		want = minf(want, _max_height)
	want = ceilf(want)
	if not is_equal_approx(custom_minimum_size.y, want):
		custom_minimum_size = Vector2(0.0, want)


## The height that still shows the header and the first three rows: the HUD shell never gives the
## list less, so the most threatening tracks stay in view when the card and weapons crowd the dock.
func least_height() -> float:
	var rows := _root.get_child_count() if _root != null else 0
	if rows <= 3 or not _table.visible:
		return _natural_height()
	# The column titles take about one row's height.
	var all_rows := _rows_height()
	var row_h := all_rows / float(rows + 1)
	return ceilf(_natural_height() - all_rows + row_h * 4.0)


func _natural_height() -> float:
	var want := _column.get_combined_minimum_size().y + float(_margin.get_theme_constant(&"margin_top") + _margin.get_theme_constant(&"margin_bottom"))
	if _table.visible:
		want += _rows_height()
	return ceilf(want)


## The table's column titles and every row, measured from the last row's rect.
func _rows_height() -> float:
	var last := _root.get_child(_root.get_child_count() - 1) if _root != null and _root.get_child_count() > 0 else null
	if last == null:
		return 0.0
	var r := _table.get_item_area_rect(last)
	var scroll := _table.get_scroll()
	var inner := float(_table.get_theme_constant(&"v_separation"))
	var panel := _table.get_theme_stylebox(&"panel")
	var frame := panel.get_margin(SIDE_TOP) + panel.get_margin(SIDE_BOTTOM) if panel != null else 0.0
	return maxf(r.end.y + scroll.y + inner + frame, 0.0)


func _count_line(counts: Dictionary, total: int, designated: String) -> String:
	var parts := PackedStringArray()
	parts.append(tr(&"hud_tracks.count.total_one" if total == 1 else &"hud_tracks.count.total").format([_num(total)]))
	for k: String in ["hostile", "suspect", "inbound"]:
		var n := Kit.get_int(counts, k, 0)
		if n > 0:
			parts.append(tr(StringName("hud_tracks.count." + k)).format([_num(n)]))
	var line := tr(&"hud_tracks.count.join").join(parts)
	if not designated.is_empty():
		line = tr(&"hud_tracks.count.with_designated").format([line, designated])
	return line


func _fill(item: TreeItem, t: Dictionary) -> void:
	var stance := clampi(Kit.get_int(t, "stance", Bp.STANCE_SUSPECT), 0, 3)
	var domain := clampi(Kit.get_int(t, "domain", Bp.DOMAIN_SURFACE), 0, 3)
	var rung := clampi(Kit.get_int(t, "rung", 0), 0, 4)
	var last_known := Kit.get_bool(t, "last_known")
	var designated := Kit.get_bool(t, "designated")
	var range_m := Kit.get_float(t, "range_m", -1.0)
	var name := Kit.get_str(t, "name")
	item.set_icon(COL_SYMBOL, Ui.icon(StringName("stance_%s_%s" % [STANCE_NAMES[stance], DOMAIN_NAMES[domain]])))
	item.set_icon_max_width(COL_SYMBOL, 24)
	item.set_icon_modulate(COL_SYMBOL, Ui.colour(&"text.secondary") if last_known else Ui.colour(STANCE_TOKENS[stance]))
	item.set_text(COL_NUMBER, Kit.get_str(t, "label") if _narrow else tr(&"hud_tracks.row.number").format([Kit.get_str(t, "label"), _word(&"stance", stance)]))
	item.set_icon(COL_ID, null if _narrow else Ui.icon(StringName("rung_%d" % rung)))
	item.set_icon_max_width(COL_ID, 24)
	item.set_icon_modulate(COL_ID, Ui.colour(&"text.secondary") if last_known else Ui.colour(&"text.primary"))
	item.set_text(COL_ID, "" if _narrow else _word(&"rung", rung))
	if range_m >= 0.0:
		item.set_text(COL_POSITION, tr(&"hud_tracks.row.position").format([_tx(&"bearing", [Kit.get_float(t, "bearing_deg", 0.0)]), _tx(&"range", [range_m])]))
	else:
		item.set_text(COL_POSITION, "")
	item.set_text_alignment(COL_POSITION, HORIZONTAL_ALIGNMENT_RIGHT)
	var tip := _tooltip(t, name, designated, last_known)
	for c in COLUMNS:
		item.set_tooltip_text(c, tip)
		item.set_auto_translate_mode(c, Node.AUTO_TRANSLATE_MODE_DISABLED)
		if last_known:
			item.set_custom_color(c, Ui.colour(&"text.secondary"))
		else:
			item.clear_custom_color(c)
		item.set_custom_font(c, _bold_font() if designated else null)
	item.set_description(COL_SYMBOL, name)
	item.set_description(COL_NUMBER, name)


func _tooltip(t: Dictionary, name: String, designated: bool, last_known: bool) -> String:
	var lines := PackedStringArray([name])
	if Kit.get_bool(t, "inbound"):
		lines.append(tr(&"hud_tracks.note.inbound"))
	if designated:
		lines.append(tr(&"hud_tracks.note.designated"))
	var engaged := Kit.get_int(t, "engaged_by", 0)
	if engaged > 0:
		lines.append((tr(&"hud_tracks.note.engaged_one") if engaged == 1 else tr(&"hud_tracks.note.engaged").format([_num(engaged)])))
	if last_known:
		lines.append(tr(&"hud_tracks.note.last_known").format([_tx(&"duration", [Kit.get_float(t, "age_s", 0.0)])]))
	return "\n".join(lines)


func _bold_font() -> Font:
	if _bold == null:
		_bold = get_theme_font(&"font", &"PanelTitle")
	return _bold


# --- selection -----------------------------------------------------------------------------------

func _on_selection_changed(track: int) -> void:
	_selected = track
	_sync_selection()


## Show the Hud selection in the table without echoing it back.
func _sync_selection() -> void:
	_syncing = true
	var item: TreeItem = _items.get(_selected)
	if item != null:
		if not item.is_selected(COL_SYMBOL):
			item.select(COL_SYMBOL)
			_table.scroll_to_item(item)
	else:
		_table.deselect_all()
	_syncing = false


func _on_item_selected() -> void:
	if _syncing:
		return
	var item := _table.get_selected()
	if item == null:
		return
	var id := int(item.get_metadata(COL_SYMBOL))
	if id != _selected:
		_selected = id
		Kit.call_on(&"Hud", &"select_track", [id])


func _on_nothing_selected() -> void:
	if _selected != -1:
		Kit.call_on(&"Hud", &"select_track", [Bp.NONE])


func _on_item_activated() -> void:
	var item := _table.get_selected()
	if item == null:
		return
	var id := int(item.get_metadata(COL_SYMBOL))
	if id > 0:
		Kit.call_on(&"Orders", &"designate", [id])


# --- sorting -------------------------------------------------------------------------------------

func _on_sort_selected(index: int) -> void:
	if index < 0 or index >= SORTS.size():
		return
	set_sort(SORTS[index], false)


func _on_title_clicked(column: int, mouse_button: int) -> void:
	if mouse_button != MOUSE_BUTTON_LEFT:
		return
	var key: StringName = &"threat"
	match column:
		COL_NUMBER:
			key = &"number"
		COL_POSITION:
			key = &"range"
	set_sort(key, not _descending if key == _sort else false)


## Sort by `&"threat"`, `&"range"`, `&"bearing"` or `&"number"`.
func set_sort(key: StringName, descending: bool) -> void:
	if not SORTS.has(key):
		key = &"threat"
	_sort = key
	_descending = descending
	_sort_button.select(SORTS.find(key))
	refresh()


func _rebuild() -> void:
	for id: int in _items.keys():
		var item: TreeItem = _items[id]
		_root.remove_child(item)
		item.free()
	_items.clear()
	_order.clear()
	refresh()


# --- text ----------------------------------------------------------------------------------------

func _word(kind: StringName, value: int) -> String:
	var v: Variant = Kit.call_on(&"Tx", &"word", [kind, value])
	return String(v) if v is String or v is StringName else ""


func _tx(method: StringName, args: Array) -> String:
	var v: Variant = Kit.call_on(&"Tx", method, args)
	return String(v) if v is String or v is StringName else ""


static func _num(n: int) -> String:
	return TranslationServer.format_number(str(n), TranslationServer.get_locale())
