extends Control
## Sortie summary for skirmish, sandbox and PvP.
## DESIGN.md L73. Unit: ui-after-action.
## Reads and calls: Session.last_summary, Session.leave.
##
## Pushed over the ended sortie by the router (`Screens`: a non-campaign sortie that ends). The plain
## loop outside the campaign (L73) has no payout or theatre, so this is the summary alone:
## - The outcome as the title, with the mode and sortie length beside it (the bound Meta line).
## - How the sortie ended in plain words (`end_reason`, a MsgDict), first in the numbers table; left
##   out when the summary does not say.
## - Enemy tonnage disabled of the tonnage present, then each objective with Met or Not met as
##   glyph and word (texts from `Hud.objectives()`, which keeps the last sortie's list).
## - The numbers a designer iterates on (L73: build, splash, fight, iterate): gunnery and missile
##   hits against shots, torpedoes, decoys and missiles seduced, fires aboard, first detection,
##   time radiating, own cells lost, alerts raised and kills by method.
## - The one tip the summary carries, if any.
## Actions: PvP, and any sortie played from a multiplayer lobby, returns to the lobby (Session.leave(true))
## as its first and tallest button, with leaving beside it; every other mode returns to the menu. Replays are opened from the
## replay browser. Leaving puts the session back to idle and the router moves the screens. Esc does
## not leave: it moves focus to the first action, so a stray key never drops the result.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

## `Bp` values used here, kept so the script parses before the extension loads.
const NONE := -1
const KIND_NONE := 0
const KIND_CAMPAIGN := 1
const KIND_SKIRMISH := 2
const KIND_SANDBOX := 3
const KIND_PVP := 4
const MODE_WORD_BY_KIND := {1: 0, 2: 1, 3: 2, 4: 3, 5: 5, 6: 6}
const PANEL_MIN_W := 820.0

@onready var _scrim: ColorRect = $Scrim
@onready var _panel: PanelContainer = $Scroll/Center/Panel
@onready var _title: Label = $Scroll/Center/Panel/Margin/Column/Header/Title
@onready var _meta: Label = $Scroll/Center/Panel/Margin/Column/Header/Meta
@onready var _lead: Label = $Scroll/Center/Panel/Margin/Column/Lead
@onready var _objectives: VBoxContainer = $Scroll/Center/Panel/Margin/Column/Objectives
@onready var _stats: VBoxContainer = $Scroll/Center/Panel/Margin/Column/Stats
@onready var _tip: Label = $Scroll/Center/Panel/Margin/Column/Tip
@onready var _secondary: Button = $Scroll/Center/Panel/Margin/Column/Actions/Secondary
@onready var _primary: Button = $Scroll/Center/Panel/Margin/Column/Actions/Primary

var _summary: Dictionary = {}
var _kind := KIND_NONE
var _objective_rows: Array[Dictionary] = []
var _live := false
var _leaving := false


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_STOP
	Ui.set_margins($Scroll/Center/Panel/Margin, 5)
	Ui.set_separation($Scroll/Center/Panel/Margin/Column, 3)
	Ui.set_separation($Scroll/Center/Panel/Margin/Column/Header, 4)
	Ui.set_separation(_objectives, 1)
	Ui.set_separation(_stats, 1)
	Ui.set_separation($Scroll/Center/Panel/Margin/Column/Actions, 3)
	$Scroll/Center/Panel/Margin/Column.custom_minimum_size = Vector2(PANEL_MIN_W, 0.0)
	for l: Label in [_title, _lead, _tip]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_primary.custom_minimum_size = Vector2(320.0, Kit.PRIMARY_H)
	_secondary.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_apply_scrim()
	Ui.changed.connect(_on_ui_changed)
	_primary.pressed.connect(_on_primary)
	_secondary.pressed.connect(_on_secondary)
	_panel.accessibility_name = tr(&"sortie_end.accessible_name")
	scroll_by_keys($Scroll)
	show_summary({}, KIND_NONE, [])


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		show_summary(_summary, _kind, _objective_rows)


func setup(_args: Dictionary) -> void:
	_live = true
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"sortie_ended") and not session.is_connected(&"sortie_ended", _on_sortie_ended):
		session.connect(&"sortie_ended", _on_sortie_ended)
	refresh()


func default_focus() -> Control:
	return _primary


## Esc over this pushed screen: never leave by accident; put focus on the first action.
func back_requested() -> void:
	Ui.focus(_primary)


func refresh() -> void:
	var info := Kit.as_dict(Kit.call_on(&"Session", &"info"))
	var summary := Kit.as_dict(Kit.call_on(&"Session", &"last_summary"))
	show_summary(summary, Kit.get_int(info, "kind", KIND_NONE), Kit.dicts(Kit.call_on(&"Hud", &"objectives")))


## Draw a `SummaryDict` for a session of `kind` (`Bp.KIND_*`). `objectives` are `ObjectiveDict`s
## that name the summary's objective ids. Also the test and capture entry point: it calls no facade
## except Tx for formatting.
func show_summary(summary: Dictionary, kind: int, objectives: Array[Dictionary]) -> void:
	_summary = summary
	_kind = kind
	_objective_rows = objectives
	Kit.clear(_objectives)
	Kit.clear(_stats)
	# Any sortie played from a multiplayer lobby goes back to it together (a co-op skirmish too), so the group
	# can go again without a new join code; leaving the lobby is the second choice.
	var lobby := _back_to_lobby()
	_primary.text = "sortie_end.to_lobby" if lobby else "sortie_end.to_menu"
	_secondary.text = "sortie_end.leave_match" if kind == KIND_PVP else "sortie_end.leave_lobby"
	_secondary.visible = lobby
	if summary.is_empty():
		_title.text = tr(&"sortie_end.title.none")
		_meta.call(&"show_value", mode_word(kind))
		_lead.text = tr(&"sortie_end.empty")
		_tip.visible = false
		_objectives.visible = false
		_link_focus()
		return
	var outcome := Kit.get_int(summary, "outcome", NONE)
	var word := _tx_str(&"word", [&"outcome", outcome])
	_title.text = word if not word.is_empty() else tr(&"sortie_end.title.none")
	var duration := _tx_str(&"clock", [Kit.get_float(summary, "duration_s")])
	var mode := mode_word(kind)
	_meta.call(&"show_value", tr(&"sortie_end.meta").format([mode, duration]) if not mode.is_empty() else duration)
	var stats := Kit.as_dict(summary.get("stats"))
	_lead.text = lead_text(stats)
	_build_objectives(Kit.dicts(summary.get("objectives")), objectives)
	_facts(_stats, end_rows(summary) + stat_rows(stats))
	var tip := _Text.resolve(summary.get("tip"))
	_tip.text = tip
	_tip.visible = not tip.is_empty()
	_link_focus()


## The mode word for a session kind (`Tx.word("mode")`), or "".
static func mode_word(kind: int) -> String:
	if not MODE_WORD_BY_KIND.has(kind):
		return ""
	return _tx_str(&"word", [&"mode", int(MODE_WORD_BY_KIND[kind])])


## "Disabled 410 t of the 640 t present (64%)." or a plain line without enemy tonnage.
static func lead_text(stats: Dictionary) -> String:
	var total := Kit.get_float(stats, "enemy_tonnage_total_t")
	if total <= 0.0:
		return String(TranslationServer.translate(&"sortie_end.lead.no_enemy"))
	var disabled := clampf(Kit.get_float(stats, "enemy_tonnage_disabled_t"), 0.0, total)
	return String(TranslationServer.translate(&"sortie_end.lead.tonnage")).format([
		Kit.number(disabled, &"t", 0), Kit.number(total, &"t", 0), Kit.percent(disabled / total)])


func _build_objectives(done: Array[Dictionary], named: Array[Dictionary]) -> void:
	_objectives.visible = not done.is_empty()
	for i in done.size():
		var o := done[i]
		var id := Kit.get_int(o, "id", NONE)
		var text := ""
		for n: Dictionary in named:
			if Kit.get_int(n, "id", NONE) == id:
				text = _Text.resolve(n.get("text"))
		if text.is_empty():
			text = tr(&"sortie_end.objective").format([Kit.number(i + 1, &"none", 0)])
		var met := Kit.get_bool(o, "done")
		var row := Kit.hbox(2)
		var mark := Kit.hbox(1)
		mark.custom_minimum_size = Vector2(Ui.spacing(6) * 2.75, 0.0)
		var word := Kit.label(tr(&"sortie_end.met") if met else tr(&"sortie_end.missed"), &"SeverityPass" if met else &"SeverityAdvisory", true)
		word.autowrap_mode = TextServer.AUTOWRAP_OFF
		mark.add_child(word)
		if met:
			var icon := TextureRect.new()
			icon.texture = Ui.icon(&"check_pass")
			icon.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
			icon.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
			icon.custom_minimum_size = Vector2(24.0, 24.0)
			icon.size_flags_vertical = Control.SIZE_SHRINK_CENTER
			mark.add_child(icon)
		row.add_child(mark)
		var l := Kit.label(text, &"Body", true)
		l.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		row.add_child(l)
		_objectives.add_child(row)


## How the sortie ended, as one `[term, value]` row, or none when the summary does not say.
static func end_rows(summary: Dictionary) -> Array:
	var why := _Text.resolve(summary.get("end_reason"))
	return [[String(TranslationServer.translate(&"sortie_end.ended")), why]] if not why.is_empty() else []


## Rows `[term, value]` of a `StatsDict`; a weapon nobody used is left out.
static func stat_rows(s: Dictionary) -> Array:
	var t := func(k: StringName) -> String: return String(TranslationServer.translate(k))
	var rows: Array = []
	var hits := func(hit_key: String, fired_key: String) -> String:
		var fired := Kit.get_int(s, fired_key)
		if fired <= 0:
			return ""
		var hit := clampi(Kit.get_int(s, hit_key), 0, fired)
		return t.call(&"sortie_end.stat.hits_value").format([Kit.number(hit, &"none", 0), Kit.number(fired, &"none", 0), Kit.percent(float(hit) / float(fired))])
	rows.append([t.call(&"sortie_end.stat.guns"), hits.call("rounds_hit", "rounds_fired")])
	rows.append([t.call(&"sortie_end.stat.missiles"), hits.call("missiles_hit", "missiles_fired")])
	rows.append([t.call(&"sortie_end.stat.torpedoes"), hits.call("torpedoes_hit", "torpedoes_fired")])
	var decoys := Kit.get_int(s, "decoys_fired")
	if decoys > 0:
		rows.append([t.call(&"sortie_end.stat.decoys"), t.call(&"sortie_end.stat.decoys_value").format([Kit.number(decoys, &"none", 0), Kit.number(Kit.get_int(s, "missiles_seduced"), &"none", 0)])])
	var first := Kit.get_float(s, "first_detection_s", -1.0)
	rows.append([t.call(&"sortie_end.stat.first_detection"), _tx_str(&"clock", [first]) if first >= 0.0 else t.call(&"sortie_end.stat.never")])
	rows.append([t.call(&"sortie_end.stat.radiating"), _tx_str(&"duration", [Kit.get_float(s, "radiating_s")])])
	rows.append([t.call(&"sortie_end.stat.fires"), Kit.number(Kit.get_int(s, "fires"), &"none", 0)])
	rows.append([t.call(&"sortie_end.stat.cells"), Kit.percent(clampf(Kit.get_float(s, "own_cells_lost"), 0.0, 1.0))])
	rows.append([t.call(&"sortie_end.stat.alerts"), Kit.number(Kit.get_int(s, "alerts_raised"), &"none", 0)])
	var kills := PackedStringArray()
	for k: Dictionary in Kit.dicts(s.get("kills_by_method")):
		if Kit.get_int(k, "count") > 0:
			kills.append(t.call(&"sortie_end.stat.kill_entry").format([_tx_str(&"word", [&"kill", Kit.get_int(k, "method")]), Kit.number(Kit.get_int(k, "count"), &"none", 0)]))
	rows.append([t.call(&"sortie_end.stat.kills"), ", ".join(kills) if not kills.is_empty() else t.call(&"sortie_end.stat.no_kills")])
	return rows


func _facts(parent: Control, rows: Array) -> void:
	var grid := GridContainer.new()
	grid.columns = 2
	grid.add_theme_constant_override(&"h_separation", Ui.spacing(4))
	grid.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	for row: Variant in rows:
		if not row is Array or (row as Array).size() < 2 or String((row as Array)[1]).is_empty():
			continue
		var term := Kit.label(String((row as Array)[0]), &"Dense", true)
		term.autowrap_mode = TextServer.AUTOWRAP_OFF
		grid.add_child(term)
		var value := Kit.label(String((row as Array)[1]), &"Body", true)
		value.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		grid.add_child(value)
	if grid.get_child_count() > 0:
		parent.add_child(grid)
	else:
		grid.free()


# --- actions -------------------------------------------------------------------------------------

func _on_primary() -> void:
	if _leaving or not _live:
		return
	_leaving = true
	Kit.call_on(&"Session", &"leave", [_back_to_lobby()])


## True when this sortie came from a multiplayer lobby that is still open: PvP always does, and a co-op
## skirmish or sandbox does when `Lobby` holds a lobby.
func _back_to_lobby() -> bool:
	return _kind == KIND_PVP or Kit.call_on(&"Lobby", &"is_active") == true


func _on_secondary() -> void:
	if _leaving or not _live:
		return
	_leaving = true
	Kit.call_on(&"Session", &"leave", [false])


func _on_sortie_ended(_summary_dict: Dictionary) -> void:
	if not _leaving:
		refresh()


func _apply_scrim() -> void:
	_scrim.color = Ui.scrim_colour()


func _on_ui_changed(what: StringName) -> void:
	if what == &"transparency" or what == &"palette":
		_apply_scrim()


## The action row, with the scrolling page above it (Up from any button scrolls the summary).
func _link_focus() -> void:
	var scroll: ScrollContainer = $Scroll
	var row: Array[Control] = []
	for b: Button in [_secondary, _primary]:
		if b.visible:
			row.append(b)
	Kit.chain_horizontal(row)
	scroll.focus_neighbor_bottom = scroll.get_path_to(_primary)
	scroll.focus_neighbor_top = scroll.get_path_to(scroll)
	for b: Control in row:
		b.focus_neighbor_top = b.get_path_to(scroll)
		b.focus_neighbor_bottom = b.get_path_to(b)
		b.focus_next = b.get_path_to(row[(row.find(b) + 1) % row.size()])
		b.focus_previous = b.get_path_to(row[(row.find(b) - 1 + row.size()) % row.size()])


## Make a scroll a focus stop that moves with Up and Down (keys, D-pad, stick) and pages with Page Up
## and Page Down, so everything in it reads without a mouse (L1738). At either end the press is left
## alone, so focus moves on to the neighbour as usual.
static func scroll_by_keys(scroll: ScrollContainer) -> void:
	scroll.focus_mode = Control.FOCUS_ALL
	scroll.gui_input.connect(func(event: InputEvent) -> void:
		var line := float(Ui.font_size(&"Body")) * 3.0
		var delta := 0.0
		if event.is_action_pressed(&"ui_down", true, true):
			delta = line
		elif event.is_action_pressed(&"ui_up", true, true):
			delta = -line
		elif event.is_action_pressed(&"ui_page_down", true, true):
			delta = scroll.size.y * 0.9
		elif event.is_action_pressed(&"ui_page_up", true, true):
			delta = -scroll.size.y * 0.9
		if delta == 0.0:
			return
		var before := scroll.scroll_vertical
		scroll.scroll_vertical = before + int(delta)
		if scroll.scroll_vertical != before:
			scroll.accept_event())


static func _tx_str(method: StringName, args: Array) -> String:
	var v: Variant = Kit.call_on(&"Tx", method, args)
	return String(v) if v is String or v is StringName else ""
