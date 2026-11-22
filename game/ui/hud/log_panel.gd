extends VBoxContainer
## Log: full history, filters, unread marks, centre on click.
## DESIGN.md L1671. Unit: ui-hud-alerts.
## Reads and calls: Hud.log_entries, Hud.log_added, Hud.log_mark_read, BpPlotView.centre_on.
##
## HUD rule 15: the log keeps the full history, filters by type and unit, marks unread, centres on
## click and takes routine events silently.
## - History: the panel lives as long as the sortie HUD, takes the whole log once (up to the
##   largest page, 5,000 entries) and then appends every `Hud.log_added` entry, so it holds every
##   line of the sortie even though one `log_entries` page holds at most 5,000. A sequence number
##   that goes backwards (a replay seek or a new sortie) means the log started again: it re-reads.
## - Rows: an unread mark (a dot glyph, never colour alone; routine entries are never marked),
##   the sortie clock and the sentence. Routine entries are drawn in text.secondary.
## - Filters: type (`Bp.LOG_*`), unit (own ship or one track, from the tracks the log names) and
##   unread only. They hold for the sortie.
## - Unread: with no filter narrowing the list, entries count as read once the open panel has
##   shown them for a moment (`READ_DWELL_S`); `Hud.log_mark_read` marks everything up to a
##   sequence number, so a filtered list marks nothing by itself. Mark all read marks every entry.
## - Click a row, or Enter on it: switch to the plot and centre on what the entry is about
##   (`centre_on_log` of the sortie HUD: its position, else its track). Arrow keys only select.
## - States: waiting (no sortie), nothing logged yet, no entry matches the filters, populated.
## Other scripts may call `show_kind(kind)` (the alert strip's "+N" sets the type filter) and
## `show_entries(entries)` (tests and captures with hand-built `LogEntryDict`s). The panel joins
## the `hud_log_panel` group.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

## Largest page `Hud.log_entries` returns.
const PAGE := 5000
## Seconds an open panel shows new entries before they count as read.
const READ_DWELL_S := 1.5
const COL_MARK := 0
const COL_TIME := 1
const COL_TEXT := 2
const MARK_W := 32
const TIME_W := 100
const NONE := -1
## `Bp.LOG_*` in order, with their filter words.
const KIND_COUNT := 11
const KIND_KEYS: Array[StringName] = [
	&"hud_alerts.log.kind.track", &"hud_alerts.log.kind.engagement", &"hud_alerts.log.kind.damage",
	&"hud_alerts.log.kind.officer", &"hud_alerts.log.kind.control", &"hud_alerts.log.kind.alert",
	&"hud_alerts.log.kind.pacing", &"hud_alerts.log.kind.objective", &"hud_alerts.log.kind.network",
	&"hud_alerts.log.kind.ace", &"hud_alerts.log.kind.system",
]
## Unit filter ids in the OptionButton: everything, the own ship, then 1000 + track number.
const UNIT_ALL := 0
const UNIT_OWN := 1
const UNIT_TRACK_BASE := 1000

@onready var _filters: HFlowContainer = $Filters
@onready var _kind_button: OptionButton = $Filters/Kind
@onready var _unit_button: OptionButton = $Filters/Unit
@onready var _unread_box: CheckBox = $Filters/Unread
@onready var _counts: HBoxContainer = $Counts
@onready var _summary: Label = $Counts/Summary
@onready var _mark_read: Button = $Counts/MarkRead
@onready var _copy: Button = $Counts/Copy
@onready var _table: Tree = $Table
@onready var _empty: Label = $Empty

## Every entry of the sortie, ascending seq.
var _all: Array[Dictionary] = []
var _last_seq := 0
## Tracks the log names, for the unit filter.
var _tracks: Dictionary[int, bool] = {}
var _kind := NONE
var _unit := UNIT_ALL
var _unread_only := false
var _root: TreeItem
var _live := false
var _dwell: Timer
var _own_vessel := NONE


func _ready() -> void:
	add_to_group(&"hud_log_panel")
	Ui.set_separation(self, 2)
	Ui.set_separation(_counts, 2)
	_filters.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	_filters.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	_summary.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for b: Control in [_kind_button, _unit_button, _unread_box, _mark_read, _copy]:
		b.focus_mode = Control.FOCUS_ALL
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_copy.icon = Ui.icon(&"copy")
	_table.allow_search = false
	_table.allow_reselect = true
	_table.select_mode = Tree.SELECT_ROW
	_table.focus_mode = Control.FOCUS_ALL
	_table.hide_root = true
	_root = _table.create_item()
	_table.item_mouse_selected.connect(_on_item_clicked)
	_table.item_activated.connect(_on_item_activated)
	_kind_button.item_selected.connect(_on_kind_selected)
	_unit_button.item_selected.connect(_on_unit_selected)
	_unread_box.toggled.connect(_on_unread_toggled)
	_mark_read.pressed.connect(mark_all_read)
	_copy.pressed.connect(_copy_log)
	_dwell = Timer.new()
	_dwell.one_shot = true
	_dwell.wait_time = READ_DWELL_S
	_dwell.timeout.connect(_mark_shown_read)
	add_child(_dwell)
	visibility_changed.connect(_on_visibility_changed)
	_setup_columns()
	_render_static_text()
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		_live = true
		if hud.has_signal(&"log_added"):
			hud.connect(&"log_added", _on_log_added)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: reload())
	reload()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render_static_text()
		_setup_columns()
		_rebuild()


func _render_static_text() -> void:
	_kind_button.clear()
	_kind_button.add_item(tr(&"hud_alerts.log.kind.all"), 0)
	for k in KIND_COUNT:
		_kind_button.add_item(tr(KIND_KEYS[k]), k + 1)
	_kind_button.select(_kind_button.get_item_index(_kind + 1))
	_kind_button.tooltip_text = tr(&"hud_alerts.log.kind.tip")
	_kind_button.accessibility_name = tr(&"hud_alerts.log.kind.name")
	_unit_button.tooltip_text = tr(&"hud_alerts.log.unit.tip")
	_unit_button.accessibility_name = tr(&"hud_alerts.log.unit.name")
	_fill_units()
	_unread_box.text = tr(&"hud_alerts.log.unread_only")
	_mark_read.text = tr(&"hud_alerts.log.mark_read")
	_copy.tooltip_text = tr(&"hud_alerts.log.copy")
	_copy.accessibility_name = tr(&"hud_alerts.log.copy")
	_table.accessibility_name = tr(&"hud_alerts.log.access")


func _setup_columns() -> void:
	_table.set_column_expand(COL_MARK, false)
	_table.set_column_custom_minimum_width(COL_MARK, MARK_W)
	_table.set_column_expand(COL_TIME, false)
	_table.set_column_custom_minimum_width(COL_TIME, TIME_W)
	_table.set_column_expand(COL_TEXT, true)
	_table.set_column_clip_content(COL_TEXT, true)


# --- data ----------------------------------------------------------------------------------------

## Read the whole log again (a new sortie, a seek, or the panel's first start).
func reload() -> void:
	if not is_node_ready():
		return
	var entries: Array[Dictionary] = []
	if _live:
		entries = Kit.dicts(Kit.call_on(&"Hud", &"log_entries", [{}, 0, PAGE]))
	_own_vessel = _read_own_vessel()
	show_entries(entries)


## Replace the history with these `LogEntryDict`s (ascending seq) and redraw.
func show_entries(entries: Array[Dictionary]) -> void:
	_all = []
	_tracks.clear()
	_last_seq = 0
	for e: Dictionary in entries:
		var seq := Kit.get_int(e, "seq", 0)
		if seq <= _last_seq:
			continue
		_all.append(e)
		_last_seq = seq
		_note_unit(e)
	_fill_units()
	_rebuild()


func _on_log_added(entry: Dictionary) -> void:
	var seq := Kit.get_int(entry, "seq", 0)
	if seq <= 0:
		return
	if seq <= _last_seq:
		# The log started again (a seek or a new sortie): read it whole.
		reload()
		return
	_all.append(entry)
	_last_seq = seq
	if _note_unit(entry):
		_fill_units()
	if _admits(entry):
		var at_end := _at_end()
		_add_row(entry)
		if at_end:
			_scroll_to_end.call_deferred()
		_show_state()
	_update_summary()
	_arm_dwell()


func _note_unit(e: Dictionary) -> bool:
	var t := Kit.get_int(e, "track", NONE)
	if t > 0 and not _tracks.has(t):
		_tracks[t] = true
		return true
	return false


func _read_own_vessel() -> int:
	var m := Kit.as_dict(Kit.call_on(&"Hud", &"motion"))
	if m.has("vessel"):
		return Kit.get_int(m, "vessel", NONE)
	return Kit.get_int(Kit.as_dict(Kit.call_on(&"Session", &"info")), "vessel", NONE)


func _admits(e: Dictionary) -> bool:
	if _kind != NONE and Kit.get_int(e, "kind", NONE) != _kind:
		return false
	if _unread_only and not _is_unread(e):
		return false
	match _unit:
		UNIT_ALL:
			pass
		UNIT_OWN:
			var v := Kit.get_int(e, "vessel", NONE)
			if Kit.get_int(e, "part", NONE) == NONE and (v == NONE or v != _own_vessel):
				return false
		_:
			if Kit.get_int(e, "track", NONE) != _unit - UNIT_TRACK_BASE:
				return false
	return true


static func _is_unread(e: Dictionary) -> bool:
	return Kit.get_bool(e, "unread") and not Kit.get_bool(e, "silent")


# --- rows ----------------------------------------------------------------------------------------

func _rebuild() -> void:
	if not is_node_ready():
		return
	_table.clear()
	_root = _table.create_item()
	for e: Dictionary in _all:
		if _admits(e):
			_add_row(e)
	_scroll_to_end.call_deferred()
	_show_state()
	_update_summary()
	_arm_dwell()


func _add_row(e: Dictionary) -> void:
	var item := _table.create_item(_root)
	item.set_metadata(COL_MARK, Kit.get_int(e, "seq", 0))
	var unread := _is_unread(e)
	if unread:
		item.set_icon(COL_MARK, Ui.icon(&"dot"))
		item.set_icon_max_width(COL_MARK, 16)
		item.set_tooltip_text(COL_MARK, tr(&"hud_alerts.log.unread"))
	var time := _clock(Kit.get_float(e, "time_s", 0.0))
	item.set_text(COL_TIME, time)
	var sentence := _Text.resolve(e.get("text", {}))
	item.set_text(COL_TEXT, sentence)
	item.set_autowrap_mode(COL_TEXT, TextServer.AUTOWRAP_WORD_SMART)
	if Kit.get_bool(e, "silent"):
		item.set_custom_color(COL_TIME, Ui.colour(&"text.secondary"))
		item.set_custom_color(COL_TEXT, Ui.colour(&"text.secondary"))
	var target := _has_target(e)
	if target:
		item.set_tooltip_text(COL_TEXT, tr(&"hud_alerts.log.centre_tip"))
	else:
		item.set_tooltip_text(COL_TEXT, " ")
	var spoken := tr(&"hud_alerts.log.row_access").format([time, sentence])
	if unread:
		spoken = tr(&"hud_alerts.log.row_unread_access").format([time, sentence])
	item.set_description(COL_TEXT, spoken)


func _show_state() -> void:
	var rows := _root.get_child_count()
	var waiting := not _live
	_table.visible = rows > 0
	_empty.visible = rows == 0
	if rows == 0:
		if waiting:
			_empty.text = tr(&"hud_alerts.log.waiting")
		elif _all.is_empty():
			_empty.text = tr(&"hud_alerts.log.empty")
		else:
			_empty.text = tr(&"hud_alerts.log.no_match")


func _update_summary() -> void:
	var unread := 0
	for e: Dictionary in _all:
		if _is_unread(e):
			unread += 1
	var shown := _root.get_child_count() if _root != null else 0
	var text := tr(&"hud_alerts.log.count").format([_number(_all.size())])
	if shown != _all.size():
		text = tr(&"hud_alerts.log.count_filtered").format([_number(shown), _number(_all.size())])
	if unread > 0:
		text = tr(&"hud_alerts.log.count_unread").format([text, _number(unread)])
	_summary.text = text
	_mark_read.disabled = unread == 0
	_link_focus()


## Explicit neighbours (L1738): the filter row, then the count row, then the table.
func _link_focus() -> void:
	Kit.chain_horizontal([_kind_button, _unit_button, _unread_box])
	Kit.chain_horizontal([_mark_read, _copy])
	var first_count: Control = _copy if _mark_read.disabled else _mark_read
	for c: Control in [_kind_button, _unit_button, _unread_box]:
		c.focus_neighbor_bottom = c.get_path_to(first_count)
		c.focus_neighbor_top = c.get_path_to(c)
	for c: Control in [_mark_read, _copy]:
		c.focus_neighbor_top = c.get_path_to(_kind_button)
		c.focus_neighbor_bottom = c.get_path_to(_table if _table.visible else c)
	_table.focus_neighbor_top = _table.get_path_to(first_count)


func _fill_units() -> void:
	if not is_node_ready():
		return
	_unit_button.clear()
	_unit_button.add_item(tr(&"hud_alerts.log.unit.all"), UNIT_ALL)
	_unit_button.add_item(tr(&"hud_alerts.log.unit.own"), UNIT_OWN)
	var ids: Array[int] = []
	for t: int in _tracks:
		ids.append(t)
	ids.sort()
	for t: int in ids:
		_unit_button.add_item(tr(&"hud_alerts.log.unit.track").format([_track_label(t)]), UNIT_TRACK_BASE + t)
	var index := _unit_button.get_item_index(_unit)
	if index < 0:
		_unit = UNIT_ALL
		index = 0
	_unit_button.select(index)


func _scroll_to_end() -> void:
	var n := _root.get_child_count() if _root != null else 0
	if n > 0:
		_table.scroll_to_item(_root.get_child(n - 1))
		# Wrapped rows take their height in the next layout pass: scroll once more after it.
		if not is_inside_tree():
			return
		await get_tree().process_frame
		n = _root.get_child_count() if _root != null else 0
		if n > 0 and is_instance_valid(_table):
			_table.scroll_to_item(_root.get_child(n - 1))


## True unless the player has picked an older row: new entries then keep the table at the newest.
func _at_end() -> bool:
	var sel := _table.get_selected()
	return sel == null or sel.get_next() == null


# --- filters -------------------------------------------------------------------------------------

## Show one type of entry (`Bp.LOG_*`), or everything with `Bp.NONE`.
func show_kind(kind: int) -> void:
	_kind = kind if kind >= 0 and kind < KIND_COUNT else NONE
	if is_node_ready():
		_kind_button.select(_kind_button.get_item_index(_kind + 1))
		_rebuild()


func _on_kind_selected(index: int) -> void:
	_kind = _kind_button.get_item_id(index) - 1
	_rebuild()


func _on_unit_selected(index: int) -> void:
	_unit = _unit_button.get_item_id(index)
	_own_vessel = _read_own_vessel() if _live else _own_vessel
	_rebuild()


func _on_unread_toggled(on: bool) -> void:
	_unread_only = on
	_rebuild()


# --- read marks ----------------------------------------------------------------------------------

func _on_visibility_changed() -> void:
	if is_visible_in_tree():
		_arm_dwell()
	else:
		_dwell.stop()


func _arm_dwell() -> void:
	if is_node_ready() and is_visible_in_tree() and not _filtered() and _has_unread_shown() and _dwell.is_stopped():
		_dwell.start()


func _has_unread_shown() -> bool:
	var item := _root.get_first_child() if _root != null else null
	while item != null:
		if item.get_icon(COL_MARK) != null:
			return true
		item = item.get_next()
	return false


## True while a type or unit filter hides entries (unread only hides nothing unread).
func _filtered() -> bool:
	return _kind != NONE or _unit != UNIT_ALL


## The panel showed its rows for the dwell time: they are read.
func _mark_shown_read() -> void:
	if not is_visible_in_tree() or _filtered():
		return
	var newest := 0
	var item := _root.get_first_child()
	while item != null:
		newest = maxi(newest, int(item.get_metadata(COL_MARK)))
		item = item.get_next()
	_mark_read_to(newest)


## Mark every entry read.
func mark_all_read() -> void:
	_mark_read_to(_last_seq)


## Mark entries up to and including `seq` read, here and in `Hud` (which marks the same range).
func _mark_read_to(seq: int) -> void:
	if seq <= 0:
		return
	Kit.call_on(&"Hud", &"log_mark_read", [seq])
	var changed := false
	for e: Dictionary in _all:
		if Kit.get_int(e, "seq", 0) > seq:
			break
		if Kit.get_bool(e, "unread"):
			e["unread"] = false
			changed = true
	if not changed:
		return
	if _unread_only:
		_rebuild()
		return
	var item := _root.get_first_child()
	while item != null:
		if int(item.get_metadata(COL_MARK)) <= seq:
			item.set_icon(COL_MARK, null)
			item.set_tooltip_text(COL_MARK, "")
		item = item.get_next()
	_update_summary()


# --- centre on click -----------------------------------------------------------------------------

func _on_item_clicked(_pos: Vector2, button: int) -> void:
	if button != MOUSE_BUTTON_LEFT:
		return
	_centre_on(_table.get_selected())


func _on_item_activated() -> void:
	_centre_on(_table.get_selected())


func _centre_on(item: TreeItem) -> void:
	if item == null:
		return
	var seq := int(item.get_metadata(COL_MARK))
	var entry := _entry(seq)
	if entry.is_empty() or not _has_target(entry):
		return
	var hud := _hud()
	if hud != null and hud.has_method(&"centre_on_log"):
		hud.call(&"centre_on_log", entry)


static func _has_target(e: Dictionary) -> bool:
	return e.get("pos") is Vector2 or Kit.get_int(e, "track", NONE) > 0


func _entry(seq: int) -> Dictionary:
	# Entries are in ascending seq: binary search.
	var lo := 0
	var hi := _all.size() - 1
	while lo <= hi:
		var mid := (lo + hi) >> 1
		var s := Kit.get_int(_all[mid], "seq", 0)
		if s == seq:
			return _all[mid]
		if s < seq:
			lo = mid + 1
		else:
			hi = mid - 1
	return {}


func _hud() -> Node:
	var n := get_parent()
	while n != null:
		if n.has_method(&"centre_on_log"):
			return n
		n = n.get_parent()
	return null


func _copy_log() -> void:
	var text: Variant = Kit.call_on(&"Hud", &"log_text")
	if text is String and not (text as String).is_empty():
		DisplayServer.clipboard_set(text)


# --- formatting ----------------------------------------------------------------------------------

func _clock(seconds: float) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"clock"):
		return String(tx.call(&"clock", seconds))
	var s := maxi(0, int(seconds))
	return "%02d:%02d" % [s / 60, s % 60]


func _track_label(track: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"track_label"):
		return String(tx.call(&"track_label", track))
	return "%04d" % track


static func _number(n: int) -> String:
	return TranslationServer.format_number(str(n), TranslationServer.get_locale())
