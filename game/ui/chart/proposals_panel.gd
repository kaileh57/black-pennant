extends VBoxContainer
## Co-op proposals: accept, decline, host confirm.
## DESIGN.md L839. Unit: ui-chart-panels.
## Reads and calls: Campaign.proposal_changed, Campaign.respond, Campaign.confirm, Campaign.withdraw_proposal.
##
## Any player proposes a transit, refit, wait, accept, decline or Intel spend; the proposal is shown
## to everyone and commits when every connected player accepts or the host confirms (L839). Each
## open proposal shows what it does, who made it and when, who has accepted and who the vote still
## waits for, and the answers open to this player: Accept and Decline, Withdraw for the proposer,
## and Confirm for the host. The panel hides itself while nothing is open, so single-player never
## sees it. Past a few proposals the list scrolls, so the orders below always keep their room.

signal focus_changed
signal rejected(text: Variant)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
## Tallest the open proposals stand before they scroll, so the orders and tabs below keep room.
const MAX_HEIGHT := 300.0

var _source: RefCounted
var _signature := ""
var _buttons: Array[Button] = []

@onready var _title: Label = $Title
@onready var _list: VBoxContainer = $Scroll/List


func _ready() -> void:
	Ui.set_separation(self, 1)
	Ui.set_separation(_list, 3)
	_title.text = &"chart.coop.title"
	accessibility_name = tr(&"chart.coop.title")


func bind(source: RefCounted) -> void:
	_source = source
	_signature = ""


## Re-read open proposals (on `Campaign.proposal_changed` and `chart_changed`).
func refresh() -> void:
	if _source == null or not is_node_ready():
		return
	var chart: Dictionary = _source.call(&"chart")
	var proposals := Kit.dicts(chart.get("proposals"))
	if proposals.is_empty():
		if _signature != "none":
			_signature = "none"
			render(proposals, [], false, -1)
		return
	var players: Array[Dictionary] = []
	players.assign(_source.call(&"players"))
	var host: bool = _source.call(&"is_host") == true
	var sig := str([proposals, players, host])
	if sig == _signature:
		return
	_signature = sig
	render(proposals, players, host, _source.call(&"local_player"))


func render(proposals: Array[Dictionary], players: Array[Dictionary], host: bool, me: int) -> void:
	_buttons.clear()
	Kit.clear(_list)
	visible = not proposals.is_empty()
	for p: Dictionary in proposals:
		_list.add_child(_row(p, players, host, me))
	_fit_height.call_deferred()
	focus_changed.emit()


func _fit_height() -> void:
	if is_instance_valid(_list):
		($Scroll as ScrollContainer).custom_minimum_size.y = minf(_list.get_combined_minimum_size().y, MAX_HEIGHT)


func _row(p: Dictionary, players: Array[Dictionary], host: bool, me: int) -> Control:
	var box := Kit.vbox(0)
	var id := Kit.get_int(p, "id", -1)
	var by := Kit.get_int(p, "by", -1)
	box.add_child(Kit.label(_Text.resolve(Kit.as_dict(p.get("text"))), &"Body", true))
	var accepted: Array[int] = []
	var raw: Variant = p.get("accepted")
	if raw is PackedInt32Array or raw is Array:
		for v: Variant in raw:
			accepted.append(int(v))
	var yes := PackedStringArray()
	var waiting := PackedStringArray()
	for pl: Dictionary in players:
		var slot := Kit.get_int(pl, "player", -1)
		var name_text := Kit.get_str(pl, "name")
		if accepted.has(slot):
			yes.append(name_text)
		elif Kit.get_bool(pl, "connected"):
			waiting.append(name_text)
	var by_name := _name_of(players, by)
	var line := tr(&"chart.coop.made").format([by_name, _dtg(Kit.get_int(p, "made_h"))])
	if not yes.is_empty():
		line += " " + tr(&"chart.coop.accepted").format([", ".join(yes)])
	if not waiting.is_empty():
		line += " " + tr(&"chart.coop.waiting").format([", ".join(waiting)])
	box.add_child(Kit.label(line, &"Dense", true))
	var row := Kit.hbox(1)
	if by == me:
		row.add_child(_button(&"chart.coop.withdraw", &"chart.coop.withdraw_tip", func() -> void: _source.call(&"withdraw", id)))
	elif not accepted.has(me):
		row.add_child(_button(&"chart.coop.accept", &"chart.coop.accept_tip", func() -> void: _source.call(&"respond", id, true)))
		row.add_child(_button(&"chart.coop.decline", &"chart.coop.decline_tip", func() -> void: _source.call(&"respond", id, false)))
	if host:
		row.add_child(_button(&"chart.coop.confirm", &"chart.coop.confirm_tip", func() -> void: _source.call(&"confirm", id)))
	if row.get_child_count() > 0:
		box.add_child(row)
	else:
		row.free()
	return box


func _button(key: StringName, tip: StringName, act: Callable) -> Button:
	var b := Kit.inline_button(String(key))
	b.tooltip_text = tr(tip)
	b.pressed.connect(func() -> void:
		if _source != null:
			act.call()
			_signature = ""
			refresh())
	_buttons.append(b)
	return b


static func _name_of(players: Array[Dictionary], slot: int) -> String:
	for pl: Dictionary in players:
		if Kit.get_int(pl, "player", -1) == slot:
			return Kit.get_str(pl, "name")
	return ""


func focus_controls() -> Array[Control]:
	var out: Array[Control] = []
	for b: Button in _buttons:
		if is_instance_valid(b):
			out.append(b)
	return out


static func _dtg(hour: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"date_time_group"):
		return String(tx.call(&"date_time_group", hour))
	return ""
