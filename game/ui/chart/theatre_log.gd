extends VBoxContainer
## Theatre log of events by hour.
## DESIGN.md L741. Unit: ui-chart-panels.
## Reads and calls: Campaign.theatre_log, Campaign.theatre_event.
##
## The theatre's history as a table, newest first: the date-time group of the hour and what
## happened. The theatre keeps the newest events and leaves out hour ticks, interrupts and the
## supply and patrol drift, which the chart itself shows (gd-campaign). Like the sortie log (HUD
## rule 15) it filters by type, marks what arrived since the player last opened it, and a row that
## names a place selects that place on the chart and boxes it.

signal focus_changed
signal unread_changed(count: int)
signal focus_requested(target: StringName)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
## Rows kept on screen; the theatre keeps no more than this either.
const LIMIT := 200
## Filters in menu order: all, LANTERN's own, the Combine, operations and acts.
const FILTERS: Array[StringName] = [&"all", &"own", &"combine", &"operations"]
const OWN: Array[StringName] = [
	&"sextant_moved", &"sextant_placed", &"sextant_sunk", &"resources_changed", &"hull_lost", &"hull_rebuilt",
	&"refit_started", &"proposal_made", &"proposal_accepted", &"proposal_declined", &"proposal_committed",
	&"proposal_withdrawn", &"saved", &"campaign_over", &"part_unlocked", &"tier_opened",
]
const COMBINE: Array[StringName] = [
	&"lane_cut", &"lane_restored", &"depot_emptied", &"supply_changed", &"patrol_density_changed", &"alert_changed",
	&"alert_escalated", &"hunter_group_sortied", &"hunter_group_arrived", &"tally_changed", &"card_queued",
	&"card_delayed", &"card_cancelled", &"card_arrived", &"group_sighted", &"convoy_revealed", &"ace_revealed",
]
const OPERATIONS: Array[StringName] = [
	&"operation_offered", &"operation_withdrawn", &"operation_unlocked", &"clock_pushed_back", &"clock_expired",
	&"act_lost", &"act_won", &"act_started", &"act_started_retreat", &"open_theatre_opened",
]

## The last event the player has seen, kept across rebuilds of the table, by campaign.
static var _last_read: Dictionary = {}
static var _read_key := ""

var _source: RefCounted
var _events: Array[Dictionary] = []
var _unread := 0
## The live campaign's log has been read once since it was opened (its entries then count as read).
static var _primed := false
var _filter := 0
var _signature := ""

@onready var _filter_button: OptionButton = $Filter
@onready var _tree: Tree = $Tree
@onready var _empty: Label = $Empty


func _ready() -> void:
	Ui.set_separation(self, 2)
	_filter_button.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_filter_button.accessibility_name = tr(&"chart.log.filter_name")
	_filter_button.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	for f: StringName in FILTERS:
		_filter_button.add_item(tr(StringName("chart.log.filter." + String(f))))
	_filter_button.item_selected.connect(_on_filter)
	_tree.focus_mode = Control.FOCUS_ALL
	_tree.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_tree.accessibility_name = tr(&"chart.tab.log")
	_tree.set_column_title(0, tr(&"chart.log.time"))
	_tree.set_column_title(1, tr(&"chart.log.event"))
	_tree.set_column_expand(0, false)
	_tree.set_column_clip_content(0, false)
	_fit_time_column()
	_tree.set_column_expand(1, true)
	_tree.item_selected.connect(_on_item_selected)
	_empty.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	accessibility_name = tr(&"chart.tab.log")


## The time column fits a date-time group plus the unread mark, at the Tree's font and size.
func _fit_time_column() -> void:
	var font := _tree.get_theme_font(&"font")
	var fs := _tree.get_theme_font_size(&"font_size")
	var w := font.get_string_size(_dtg(0) if not _dtg(0).is_empty() else "000000Z MMM", HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x if font != null else 150.0
	var icon_w := 24.0
	var pad := float(_tree.get_theme_constant(&"h_separation")) * 2.0 + float(_tree.get_theme_constant(&"item_margin"))
	_tree.set_column_custom_minimum_width(0, ceili(w + icon_w + pad + 8.0))


func bind(source: RefCounted) -> void:
	_source = source
	_signature = ""
	var key := Kit.get_str(Kit.as_dict(source.call(&"current_slot")), "campaign_id") if source != null else ""
	if key != _read_key:
		_read_key = key
		_last_read = {}
		_primed = false


## Re-read the log (on `Campaign.theatre_event`, folded to once a frame by the table).
func refresh() -> void:
	if _source == null or not is_node_ready():
		return
	var events: Array[Dictionary] = []
	events.assign(_source.call(&"theatre_log", LIMIT))
	var sig := str(events.size()) + (str(events[-1]) if not events.is_empty() else "")
	if sig == _signature:
		return
	_signature = sig
	_events = events
	# What the log already held when this campaign was opened is not news: "new" counts what happens
	# while the player is at the table (a fresh campaign would otherwise open on ten unread entries).
	if is_visible_in_tree() or (not _primed and _source.call(&"is_live") == true):
		_mark_read_quietly()
	_primed = true
	_count_unread()
	_render()


func unread_count() -> int:
	return _unread


## The player opened the log: everything shown is read.
func mark_read() -> void:
	_mark_read_quietly()
	_count_unread()
	_render()


func _mark_read_quietly() -> void:
	_last_read = _events[-1] if not _events.is_empty() else {}


func _count_unread() -> void:
	var before := _unread
	_unread = 0
	if _last_read.is_empty():
		_unread = _events.size()
	else:
		var at := -1
		for i in range(_events.size() - 1, -1, -1):
			if _events[i] == _last_read:
				at = i
				break
		# The last read event fell out of the kept window: everything kept is newer.
		_unread = _events.size() - 1 - at if at >= 0 else _events.size()
	if _unread != before:
		unread_changed.emit(_unread)


static func category(kind: StringName) -> StringName:
	if OWN.has(kind):
		return &"own"
	if COMBINE.has(kind):
		return &"combine"
	if OPERATIONS.has(kind):
		return &"operations"
	return &"own"


func _render() -> void:
	_tree.clear()
	var root := _tree.create_item()
	var want: StringName = FILTERS[clampi(_filter, 0, FILTERS.size() - 1)]
	var shown := 0
	var first_unread := _events.size() - _unread
	for i in range(_events.size() - 1, -1, -1):
		var e := _events[i]
		var kind := StringName(Kit.get_str(e, "kind"))
		if want != &"all" and category(kind) != want:
			continue
		var item := _tree.create_item(root)
		item.set_text(0, _dtg(Kit.get_int(e, "hour")))
		item.set_text(1, _Text.resolve(Kit.as_dict(e.get("text"))))
		item.set_autowrap_mode(1, TextServer.AUTOWRAP_WORD_SMART)
		item.set_metadata(0, target_of(e))
		if i >= first_unread:
			item.set_icon(0, Ui.icon(&"dot"))
			item.set_tooltip_text(0, tr(&"chart.log.unread"))
		shown += 1
	_empty.visible = shown == 0
	_tree.visible = shown > 0
	if _source != null and Kit.as_dict(_source.call(&"chart")).is_empty():
		_empty.text = tr(&"chart.log.no_campaign")
	else:
		_empty.text = tr(&"chart.log.empty") if want == &"all" else tr(&"chart.log.empty_filter")
	focus_changed.emit()


## The place an event is about: a node id or a region id, `&""` when none.
static func target_of(e: Dictionary) -> StringName:
	for key: String in ["node", "to", "relocated_to", "a"]:
		var v := StringName(Kit.get_str(e, key))
		if v != &"":
			return v
	return StringName(Kit.get_str(e, "region"))


func _on_filter(index: int) -> void:
	_filter = index
	_render()


func _on_item_selected() -> void:
	var item := _tree.get_selected()
	if item == null:
		return
	var target: Variant = item.get_metadata(0)
	if target is StringName and target != &"":
		focus_requested.emit(target)


func focus_controls() -> Array[Control]:
	var out: Array[Control] = [_filter_button]
	if _tree.visible:
		out.append(_tree)
	return out


static func _dtg(hour: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"date_time_group"):
		return String(tx.call(&"date_time_group", hour))
	return ""
