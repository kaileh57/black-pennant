extends Control
## L4 script node editor. Later phase.
## DESIGN.md L355-391. Unit: ui-editor-graph.
## Reads and calls: Editor.scripts, Editor.script_set_source, Editor.script_set_enabled.
##
## The dry dock's Scripts view. L4 scripts are Luau source in a graph node with typed ports, declared one
## per line in the source (`--@in name: type`, `--@out name: type`). The list on the left holds the design's
## scripts and says which are switched off; the page holds the one picked: its name, its switch, its
## declared ports, the compiler's problem if any, and the source in a code field (mono is allowed here:
## node-graph code, L1590). Edits are stored a moment after typing stops and when the field loses focus,
## each as one undo step, and checked by the compiler then.
##
## - Scripts from shared designs arrive switched off (L2123); the player switches each one on.
## - "Place in graph" adds the script as a node; it then replaces a stage's subgraph or drives a sink like
##   any player node. A script that faulted in a splash or sortie greys out for that sortie; Re-arm sets it
##   running again, in dry dock or during a splash (L371).
## - States: Luau not in this build; scripts not allowed by this dock's ruleset (L971: never in PvP);
##   no scripts yet; a script picked.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

const PORT_NAMES: Array[String] = ["bool", "scalar", "angle", "vec3", "text", "stance", "emcon", "track", "track_list",
	"detection_list", "part_state", "orders"]
## Luau words drawn as keywords (the compiler is the authority on the language).
const KEYWORDS: Array[String] = ["and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
	"local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "continue", "export", "type"]
## Typing stops for this long before the source is stored.
const STORE_AFTER_S := 0.6
## Where "Place in graph" puts the node: left of the generated columns.
const PLACE_AT := Vector2(-480.0, 0.0)

@onready var _new_name: LineEdit = $Column/Bar/Margin/Row/NewName
@onready var _add: Button = $Column/Bar/Margin/Row/Add
@onready var _rearm: Button = $Column/Bar/Margin/Row/Rearm
@onready var _status: Label = $Column/Bar/Margin/Row/Status
@onready var _list: ItemList = $Column/Body/List
@onready var _page: VBoxContainer = $Column/Body/Page/Margin/Column
@onready var _name: Label = $Column/Body/Page/Margin/Column/Head/Name
@onready var _enabled: CheckButton = $Column/Body/Page/Margin/Column/Head/Enabled
@onready var _place: Button = $Column/Body/Page/Margin/Column/Head/Place
@onready var _remove: Button = $Column/Body/Page/Margin/Column/Head/Remove
@onready var _ports: Label = $Column/Body/Page/Margin/Column/Ports
@onready var _problem: Label = $Column/Body/Page/Margin/Column/Problem
@onready var _code: CodeEdit = $Column/Body/Page/Margin/Column/Code
@onready var _empty: CenterContainer = $Column/Body/Page/Empty
@onready var _empty_text: Label = $Column/Body/Page/Empty/Text

var _scripts: Array[Dictionary] = []
var _current := -1
var _refresh_pending := false
var _stale := true
var _store_timer: Timer
## The source last stored for `_current`, so a refresh never overwrites typing.
var _stored_source := ""
var _allowed := false
## The next `_show` replaces the code field whatever it holds (a different script was picked).
var _force_show := true


func _ready() -> void:
	accessibility_name = tr(&"editor_script.name")
	Ui.set_margins($Column/Bar/Margin as MarginContainer, 1)
	Ui.set_margins($Column/Body/Page/Margin as MarginContainer, 2)
	Ui.set_separation(_page, 2)
	var row := $Column/Bar/Margin/Row as HFlowContainer
	row.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	var head := $Column/Body/Page/Margin/Column/Head as HFlowContainer
	head.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	head.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	($Column/Body as BoxContainer).add_theme_constant_override(&"separation", 0)
	($Column as BoxContainer).add_theme_constant_override(&"separation", 0)
	_new_name.accessibility_name = tr(&"editor_script.new_name")
	_new_name.text_submitted.connect(func(_t: String) -> void: _on_add())
	_add.icon = Ui.icon(&"add")
	_add.tooltip_text = tr(&"editor_script.add_tip")
	_add.pressed.connect(_on_add)
	_rearm.tooltip_text = tr(&"editor_script.rearm_tip")
	_rearm.pressed.connect(_on_rearm)
	_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_list.accessibility_name = tr(&"editor_script.list")
	_list.item_selected.connect(_on_pick)
	_list.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_name.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_ports.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_problem.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_enabled.tooltip_text = tr(&"editor_script.enabled_tip")
	_enabled.toggled.connect(_on_enabled)
	_place.tooltip_text = tr(&"editor_script.place_tip")
	_place.pressed.connect(_on_place)
	_remove.icon = Ui.icon(&"delete")
	_remove.tooltip_text = tr(&"editor_script.remove_tip")
	_remove.pressed.connect(_confirm_remove)
	_code.accessibility_name = tr(&"editor_script.code")
	_code.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_code.text_changed.connect(_on_typed)
	_code.focus_exited.connect(_store)
	_code.syntax_highlighter = _highlighter()
	_code.delimiter_comments = PackedStringArray(["--"])
	_code.delimiter_strings = PackedStringArray(["\" \"", "' '"])
	_store_timer = Timer.new()
	_store_timer.one_shot = true
	_store_timer.wait_time = STORE_AFTER_S
	_store_timer.timeout.connect(_store)
	add_child(_store_timer)
	var e := _editor()
	if e != null:
		for sig: StringName in [&"graph_changed", &"undo_changed"]:
			if e.has_signal(sig):
				e.connect(sig, _queue_refresh)
		if e.has_signal(&"design_changed"):
			e.connect(&"design_changed", func(_rev: int) -> void: _queue_refresh())
	Ui.changed.connect(func(what: StringName) -> void:
		if what == &"palette":
			_code.syntax_highlighter = _highlighter())
	visibility_changed.connect(func() -> void:
		if not is_visible_in_tree():
			_store()
		elif _stale:
			_refresh())


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_queue_refresh()


func _editor() -> Node:
	return Kit.autoload(&"Editor")


## Comments and strings in the support tone; everything else stays text.primary (hue only codes).
func _highlighter() -> CodeHighlighter:
	var h := CodeHighlighter.new()
	var quiet := Ui.colour(&"text.secondary")
	var ink := Ui.colour(&"text.primary")
	h.number_color = ink
	h.symbol_color = ink
	h.function_color = ink
	h.member_variable_color = ink
	for k in KEYWORDS:
		h.add_keyword_color(k, ink)
	h.add_color_region("--", "", quiet, true)
	h.add_color_region("\"", "\"", quiet)
	h.add_color_region("'", "'", quiet)
	return h


func _queue_refresh() -> void:
	if not is_visible_in_tree():
		_stale = true
		return
	if _refresh_pending:
		return
	_refresh_pending = true
	_refresh.call_deferred()


func _refresh() -> void:
	_refresh_pending = false
	_stale = false
	if not is_node_ready():
		return
	var e := _editor()
	var open := e != null and bool(e.call(&"is_open"))
	var app := Kit.autoload(&"App")
	var in_build := app != null and Kit.get_bool(Kit.as_dict(app.call(&"build_info")), "luau")
	var limits := Kit.as_dict(e.call(&"limits")) if open else {}
	var ruleset := Kit.as_dict(limits.get("ruleset", {}))
	_allowed = in_build and Kit.get_int(limits, "max_layer", Bp.LAYER_SCRIPT) >= Bp.LAYER_SCRIPT and Kit.get_bool(ruleset, "luau", true)
	_scripts = []
	if open and in_build:
		_scripts = Kit.dicts(e.call(&"scripts"))
	_new_name.editable = _allowed
	_add.disabled = not _allowed
	_rearm.disabled = not _allowed or _scripts.is_empty()
	_fill_list()
	var message := ""
	if not open:
		message = tr(&"editor_script.closed")
	elif not in_build:
		message = tr(&"editor_script.not_in_build")
	elif not _allowed:
		message = tr(&"editor_script.not_allowed")
	elif _scripts.is_empty():
		message = tr(&"editor_script.empty")
	_list.visible = in_build and open
	($Column/Bar as Control).visible = in_build and open
	_empty.visible = not message.is_empty()
	_empty_text.text = message
	_page.visible = message.is_empty()
	if message.is_empty():
		_current = clampi(_current, 0, _scripts.size() - 1)
		_list.select(_current)
		_show(_scripts[_current])
	else:
		_current = -1
	_chain_focus()


func _fill_list() -> void:
	_list.clear()
	for s: Dictionary in _scripts:
		var text := Kit.get_str(s, "name")
		var off := not Kit.get_bool(s, "enabled", true)
		var broken := not _Text.is_empty(s.get("error", {}))
		if off:
			text = tr(&"editor_script.list.off").format([text])
		elif broken:
			text = tr(&"editor_script.list.problem").format([text])
		var i := _list.add_item(text)
		if broken:
			_list.set_item_icon(i, Ui.icon(&"check_fail"))
			_list.set_item_icon_modulate(i, Ui.colour(&"alert.warning"))
		_list.set_item_tooltip(i, Kit.get_str(s, "name"))


## Show one script; the code field keeps the player's typing unless the stored source moved under it.
func _show(s: Dictionary) -> void:
	_name.text = Kit.get_str(s, "name")
	_enabled.set_pressed_no_signal(Kit.get_bool(s, "enabled", true))
	_enabled.disabled = not _allowed
	_place.disabled = not _allowed
	_remove.disabled = not _allowed
	_code.editable = _allowed
	var source := Kit.get_str(s, "source")
	if _force_show or source != _stored_source:
		# The stored source moved (another script picked, undo): show it unless the player is mid-edit.
		if _force_show or _code.text == _stored_source or not _code.has_focus():
			if _code.text != source:
				var caret := _code.get_caret_line()
				_code.text = source
				_code.clear_undo_history()
				_code.set_caret_line(mini(caret, maxi(_code.get_line_count() - 1, 0)))
		_stored_source = source
		_force_show = false
	_show_checks(s)


func _show_checks(s: Dictionary) -> void:
	var ins := _ports_text(Kit.dicts(s.get("inputs", [])))
	var outs := _ports_text(Kit.dicts(s.get("outputs", [])))
	_ports.text = tr(&"editor_script.ports").format([ins, outs])
	_problem.text = _Text.resolve(s.get("error", {}))
	_problem.visible = not _problem.text.is_empty()


func _ports_text(ports: Array[Dictionary]) -> String:
	if ports.is_empty():
		return tr(&"editor_script.ports.none")
	var parts: PackedStringArray = []
	for p: Dictionary in ports:
		var t := Kit.get_int(p, "type")
		var type_word := tr(StringName("editor_graph.port." + PORT_NAMES[t])) if t >= 0 and t < PORT_NAMES.size() else ""
		parts.append(tr(&"editor_script.port").format([tr(StringName(p.get("name", &""))), type_word]))
	return ", ".join(parts)


func _say(msg: Variant) -> void:
	_status.text = _Text.resolve(msg)


# --- actions ----------------------------------------------------------------------------------------

func _on_pick(i: int) -> void:
	_store()
	_current = i
	_force_show = true
	_refresh()


func _on_add() -> void:
	if not _allowed:
		return
	_store()
	var index := int(_editor().call(&"script_add", _new_name.text.strip_edges()))
	if index < 0:
		_say(&"editor_script.refused.add")
		return
	_new_name.text = ""
	_current = index
	_force_show = true
	_say(&"")
	_refresh()
	Ui.focus(_code)


func _on_typed() -> void:
	if _allowed and _current >= 0:
		_store_timer.start()


## Store the source when it differs from what is stored: one undo step, checked by the compiler.
func _store() -> void:
	_store_timer.stop()
	if not _allowed or _current < 0 or _current >= _scripts.size():
		return
	var text := _code.text
	if text == _stored_source:
		return
	var s := Kit.as_dict(_editor().call(&"script_set_source", _current, text))
	if s.is_empty():
		_say(&"editor_script.refused.store")
		return
	_stored_source = Kit.get_str(s, "source")
	_scripts[_current] = s
	_show_checks(s)
	_fill_list()
	_list.select(_current)


func _on_enabled(on: bool) -> void:
	if _current < 0:
		return
	if not bool(_editor().call(&"script_set_enabled", _current, on)):
		_say(&"editor_script.refused.enable")
		_enabled.set_pressed_no_signal(not on)
		return
	_say(&"")


func _on_place() -> void:
	if _current < 0:
		return
	_store()
	var id := int(_editor().call(&"graph_add_node", "script:%d" % _current, PLACE_AT))
	_say(&"editor_script.placed" if id >= 0 else &"editor_script.refused.place")


func _on_rearm() -> void:
	var ok := bool(_editor().call(&"script_rearm"))
	_say(&"editor_script.rearmed" if ok else &"editor_script.refused.rearm")


func _confirm_remove() -> void:
	if _current < 0:
		return
	var name := Kit.get_str(_scripts[_current], "name")
	var dialog: Node = Screens.open_dialog(&"editor_script.remove_title", tr(&"editor_script.remove_body").format([name]), [
		{"id": &"keep", "label": &"editor_script.remove_keep"},
		{"id": &"remove", "label": &"editor_script.remove_confirm"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", func(choice: StringName) -> void:
			if choice == &"remove":
				_store_timer.stop()
				if bool(_editor().call(&"script_remove", _current)):
					_current = maxi(_current - 1, 0)
					_force_show = true
					_say(&"")
				_refresh()
			Ui.focus(_remove))


func _chain_focus() -> void:
	var live := Kit.chain_vertical([_new_name, _add, _rearm, _list, _enabled, _place, _remove, _code])
	if not live.is_empty():
		live[0].focus_neighbor_top = NodePath()
		live[0].focus_previous = NodePath()
		live[-1].focus_neighbor_bottom = NodePath()
		live[-1].focus_next = NodePath()
