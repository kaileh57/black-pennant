extends Control
## Campaign slots: continue, load, new, delete; suspend slot first.
## DESIGN.md L826-828. Unit: ui-campaign-menu.
## Reads and calls: Campaign.slots, Campaign.load_slot, Campaign.resume_suspend, Campaign.delete_slot.
##
## A base screen (opened with `Screens.change`), so a load that fails returns here. One row per save,
## newest first, the suspend slot above the rest: quitting mid-sortie writes it and loading it
## deletes it (L826), so its button says Resume sortie and its line says so. The newest playable row
## gets the tall button and the first focus. A save this build cannot read stays listed, with its
## problem and no load button, so the player sees why it is not offered. Ironman saves say so.
## Delete asks first and keeps focus on the safe choice. With no save at all the screen offers one
## thing, starting a campaign.
##
## Esc returns to the main menu. Loading only reports problems: the campaign phase moves the
## screens (`Screens` routing).

const Kit := preload("res://ui/screens/menu_kit.gd")

@onready var _start: Button = $Margin/Column/Header/Start
@onready var _back: Button = $Margin/Column/Header/Back
@onready var _status: Label = $Margin/Column/Status
@onready var _list: VBoxContainer = $Margin/Column/Scroll/List

var _slots: Array[Dictionary] = []
var _focus_first: Control
## Row buttons in order, for the focus chain: `[load or null, delete]` per row.
var _row_buttons: Array[Array] = []

var _theme_started := false

func _ready() -> void:
	Ui.set_margins($Margin, 5)
	Ui.set_separation($Margin/Column, 3)
	Ui.set_separation($Margin/Column/Header, 3)
	Ui.set_separation(_list, 3)
	_status.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_status.text = ""
	_start.pressed.connect(func() -> void: Kit.go(&"campaign_new"))
	_back.pressed.connect(_leave)
	accessibility_name = tr(&"campaign_menu.slots.title")


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		show_slots(_slots)


func setup(_args: Dictionary) -> void:
	_theme_started = true
	Kit.start_menu_theme()
	refresh()


func default_focus() -> Control:
	return _focus_first if _focus_first != null and is_instance_valid(_focus_first) else _start


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		_leave()


func refresh() -> void:
	var raw: Variant = Kit.call_on(&"Campaign", &"slots")
	if not raw is Array:
		show_error()
		return
	show_slots(Kit.dicts(raw))


## The slots could not be read at all: say so, keep Start and Back.
func show_error() -> void:
	_slots = []
	Kit.clear(_list)
	_row_buttons.clear()
	_status.text = tr(&"campaign_menu.slots.error")
	_status.visible = true
	_focus_first = _start
	_link_focus()


## Draw the list from SaveSlotDicts (also the test entry point). The suspend slot goes first; the
## rest keep the order given (newest first).
func show_slots(slots: Array[Dictionary]) -> void:
	_slots = slots
	Kit.clear(_list)
	_row_buttons.clear()
	_focus_first = null
	var ordered: Array[Dictionary] = []
	for s: Dictionary in slots:
		if Kit.get_bool(s, "is_suspend"):
			ordered.append(s)
	for s: Dictionary in slots:
		if not Kit.get_bool(s, "is_suspend"):
			ordered.append(s)
	if ordered.is_empty():
		_status.text = tr(&"campaign_menu.slots.empty")
		_status.visible = true
		var start := Kit.button("campaign_menu.slots.start_first", true)
		start.pressed.connect(func() -> void: Kit.go(&"campaign_new"))
		_list.add_child(start)
		_row_buttons.append([start])
		_focus_first = start
		_link_focus()
		return
	_status.text = ""
	_status.visible = false
	var primary_done := false
	for i in ordered.size():
		var s := ordered[i]
		var playable := Kit.get_bool(s, "loadable", true)
		var primary := playable and not primary_done
		if primary:
			primary_done = true
		if i > 0:
			_list.add_child(HSeparator.new())
		_list.add_child(_build_row(s, primary))
	_link_focus()


func _build_row(slot: Dictionary, primary: bool) -> Control:
	var row := Kit.hbox(4)
	var info := Kit.vbox(0)
	info.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	var name_text := Kit.get_str(slot, "slot_name")
	if name_text.is_empty():
		name_text = tr(&"campaign_menu.slots.unnamed")
	var suspend := Kit.get_bool(slot, "is_suspend")
	if suspend:
		name_text = tr(&"campaign_menu.slots.suspend_name").format([name_text])
	info.add_child(Kit.label(name_text, &"PanelTitle", true))
	var playable := Kit.get_bool(slot, "loadable", true)
	# One support line under the name (banned-pattern table: one support line max): the summary,
	# the players, then the problem or the suspend note, as consecutive sentences. A save whose
	# header could not be read has no act, day or play time, so a summary of zeros is left out.
	var parts := PackedStringArray()
	if playable or Kit.get_int(slot, "played_s") > 0 or Kit.get_int(slot, "modified_unix") > 0:
		parts.append(summary(slot))
	var players := Kit.as_array(slot.get("players", PackedStringArray()))
	if slot.get("players") is PackedStringArray:
		players = Array(slot.get("players") as PackedStringArray)
	if players.size() > 1:
		parts.append(tr(&"campaign_menu.slots.players").format([", ".join(PackedStringArray(players))]))
	if not playable:
		var problem := Kit._Text.resolve(slot.get("problem", {}))
		parts.append(problem if not problem.is_empty() else tr(&"campaign_menu.slots.unreadable"))
	elif suspend:
		parts.append(tr(&"campaign_menu.slots.suspend_note"))
	if not parts.is_empty():
		info.add_child(Kit.label(" ".join(parts), &"Body", true))
	row.add_child(info)
	var buttons := Kit.vbox(1)
	buttons.custom_minimum_size = Vector2(280.0, 0.0)
	buttons.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	var load_button: Button = null
	if playable:
		load_button = Kit.button("campaign_menu.slots.resume" if suspend else "campaign_menu.slots.load", primary)
		load_button.accessibility_description = name_text
		load_button.pressed.connect(_load.bind(slot))
		buttons.add_child(load_button)
		if primary:
			_focus_first = load_button
	var delete := Kit.button("campaign_menu.slots.discard" if suspend else "campaign_menu.slots.delete")
	delete.accessibility_description = name_text
	delete.pressed.connect(_ask_delete.bind(slot))
	buttons.add_child(delete)
	row.add_child(buttons)
	_row_buttons.append([load_button, delete])
	if _focus_first == null and load_button == null and _row_buttons.size() == 1:
		_focus_first = delete
	return row


## One line: act, theatre day, time played, when it was saved, Ironman.
static func summary(slot: Dictionary) -> String:
	var act := Kit.get_int(slot, "act", 0)
	var act_word := ""
	if Kit.autoload(&"Tx") != null:
		act_word = String(Kit.call_on(&"Tx", &"word", [&"act", act]))
	if act_word.is_empty():
		act_word = Kit.tr_key(StringName("units.word.act.%d" % (clampi(act, 0, 3) + 1)))
	var played := float(Kit.get_int(slot, "played_s", 0))
	var played_text := ""
	if Kit.autoload(&"Tx") != null:
		played_text = String(Kit.call_on(&"Tx", &"duration", [played]))
	if played_text.is_empty():
		played_text = Kit.number(roundf(played / 60.0), &"min", 0)
	var line := Kit.tr_key(&"menu.slot.summary").format([act_word, Kit.get_int(slot, "day", 1), played_text])
	var saved := Kit.local_time(Kit.get_int(slot, "modified_unix", 0))
	if not saved.is_empty():
		line += " " + Kit.tr_key(&"menu.slot.saved").format([saved])
	if Kit.get_bool(slot, "ironman"):
		line += " " + Kit.tr_key(&"menu.slot.ironman")
	return line


func _link_focus() -> void:
	# Header (Start, Back) first, then every row button in reading order: each row stacks its load
	# button over its delete button, so Up and Down walk the list as it is drawn.
	var all: Array[Control] = []
	for pair: Array in _row_buttons:
		for b: Variant in pair:
			if b is Control:
				all.append(b)
	var live := Kit.chain_vertical(all)
	Kit.link_sides(live, null, null)
	Kit.chain_horizontal([_start, _back])
	var top: Control = live[0] if not live.is_empty() else null
	for b: Button in [_start, _back]:
		b.focus_neighbor_top = b.get_path_to(b)
		b.focus_neighbor_bottom = b.get_path_to(top if top != null else b)
	if top != null:
		top.focus_neighbor_top = top.get_path_to(_start)


# --- actions -------------------------------------------------------------------------------------

func _load(slot: Dictionary) -> void:
	var ok: bool
	if Kit.get_bool(slot, "is_suspend"):
		ok = await Kit.resume_suspend()
	else:
		ok = await Kit.load_campaign(Kit.get_str(slot, "path"))
	if not ok and is_inside_tree():
		refresh()


func _ask_delete(slot: Dictionary) -> void:
	var name_text := Kit.get_str(slot, "slot_name")
	var suspend := Kit.get_bool(slot, "is_suspend")
	var title := {"key": &"campaign_menu.discard.title" if suspend else &"campaign_menu.delete.title", "args": [name_text]}
	var body: StringName = &"campaign_menu.discard.body" if suspend else (&"campaign_menu.delete.body_ironman" if Kit.get_bool(slot, "ironman") else &"campaign_menu.delete.body")
	var dialog := Screens.open_dialog(title, body, [
		{"id": &"delete", "label": &"campaign_menu.discard.confirm" if suspend else &"campaign_menu.delete.confirm"},
		{"id": &"keep", "label": &"campaign_menu.delete.keep"},
	], &"keep", &"keep")
	if dialog != null:
		dialog.connect(&"chosen", _on_delete_chosen.bind(slot), CONNECT_ONE_SHOT)


func _on_delete_chosen(id: StringName, slot: Dictionary) -> void:
	if id != &"delete":
		return
	var ok: Variant = Kit.call_on(&"Campaign", &"delete_slot", [Kit.get_str(slot, "path")])
	refresh()
	if ok != true:
		Screens.open_message(&"campaign_menu.delete.failed_title", &"campaign_menu.delete.failed_body")
	else:
		Ui.focus.call_deferred(default_focus())


func _leave() -> void:
	Screens.change(&"main_menu")
