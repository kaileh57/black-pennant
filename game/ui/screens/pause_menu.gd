extends Control
## Pause menu over a sortie, splash or chart: resume, settings, leave (says when a suspend save is written).
## DESIGN.md L112, L826-828. Unit: ui-menu.
## Reads and calls: Session.set_paused, Session.can_suspend, Session.leave.
##
## Pushed over the sortie HUD, the Splash basin or the chart table (`Screens.push(&"pause_menu")`).
## `args.context` may name what is under it (`&"sortie"`, `&"splash"`, `&"chart"`, `&"replay"`);
## otherwise it is read from Editor, Session and Campaign.
##
## Pause is single-player only (L47, L112): `Session.set_paused` answers false in co-op and PvP, and
## the menu then says the sortie keeps running. Over the chart it holds the theatre-time animation;
## over a replay it holds playback. Whatever the menu paused it resumes on close, by Resume, Esc or
## any other way the menu leaves the stack.
##
## In a campaign sortie, Withdraw from the operation orders every player hull out: the helm makes for
## the nearest exit at full speed and the sortie ends Withdrawn there, or once clear of contact
## (`Orders.withdraw`, L537). It closes the menu and resumes, since withdrawal takes sortie time. Once
## ordered it stays ordered for that sortie; the button then says so and is disabled.
##
## Leave is worded for what it does, and the line under it says what is kept (L1697 rule 3):
## - campaign sortie, this process hosts: Save and leave, which writes the suspend save from the
##   last 60 s checkpoint (L112, L826); no question asked, nothing is lost.
## - campaign sortie as a co-op member: the officers keep the hull running and the slot is held for
##   5 minutes (L838); asks first.
## - skirmish and PvP: the result is lost; asks first. Sandbox, tutorial and replays leave at once.
## - Splash: back to the dry dock with the design kept (`Editor.splash_end`).
## - chart: saves when a full save is allowed here, otherwise asks first because progress since the
##   last full save is lost (L826).

const Kit := preload("res://ui/screens/menu_kit.gd")

## `Bp` values used here, kept so the script parses before the extension loads.
const SESSION_IN_SORTIE := 3
const SESSION_REPLAY := 5
const KIND_CAMPAIGN := 1
const KIND_SKIRMISH := 2
const KIND_SANDBOX := 3
const KIND_PVP := 4
const KIND_TUTORIAL := 6
const KIND_REPLAY := 7
const SAVE_ALLOWED := 0

@onready var _scrim: ColorRect = $Scrim
@onready var _panel: PanelContainer = $Scroll/Center/Panel
@onready var _title: Label = $Scroll/Center/Panel/Margin/Column/Title
@onready var _state: Label = $Scroll/Center/Panel/Margin/Column/State
@onready var _resume: Button = $Scroll/Center/Panel/Margin/Column/Resume
@onready var _settings: Button = $Scroll/Center/Panel/Margin/Column/Settings
@onready var _withdraw: Button = $Scroll/Center/Panel/Margin/Column/Withdraw
@onready var _withdraw_note: Label = $Scroll/Center/Panel/Margin/Column/WithdrawNote
@onready var _leave: Button = $Scroll/Center/Panel/Margin/Column/Leave
@onready var _leave_note: Label = $Scroll/Center/Panel/Margin/Column/LeaveNote

## The sortie (by its seed) in which withdrawal was ordered, so the menu says so on reopening.
static var _withdrawn_seed := -1

var _context: StringName = &"sortie"
var _seed := -2
var _kind := -1
var _paused_session := false
var _paused_clock := false
var _paused_replay := false
var _leaving := false
## What Leave does: `&"suspend"`, `&"drop"`, `&"lose"`, `&"plain"`, `&"tutorial"`, `&"replay"`,
## `&"splash"`, `&"chart_save"`, `&"chart_lose"`.
var _leave_kind: StringName = &"plain"


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_STOP
	Ui.set_margins($Scroll/Center/Panel/Margin, 5)
	Ui.set_separation($Scroll/Center/Panel/Margin/Column, 2)
	_state.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_leave_note.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_state.text = ""
	_leave_note.text = ""
	_withdraw_note.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_withdraw_note.text = ""
	_apply_scrim()
	Ui.changed.connect(_on_ui_changed)
	_resume.pressed.connect(_on_resume)
	_settings.pressed.connect(func() -> void: Screens.push(&"settings"))
	_leave.pressed.connect(_on_leave)
	_withdraw.pressed.connect(_on_withdraw)
	_chain()
	_panel.accessibility_name = tr(&"pause.title.menu")


func _exit_tree() -> void:
	if not _leaving:
		_restore()


## `args.context`: what the menu is over; read from the facades when absent.
func setup(args: Dictionary) -> void:
	var given := StringName(Kit.get_str(args, "context"))
	_context = given if given in [&"sortie", &"splash", &"chart", &"replay"] else _detect_context()
	var info := Kit.as_dict(Kit.call_on(&"Session", &"info"))
	_kind = Kit.get_int(info, "kind", -1)
	_seed = Kit.get_int(info, "seed", -2)
	_hold()
	var can_suspend: Variant = Kit.call_on(&"Session", &"can_suspend")
	var can_save: Variant = Kit.call_on(&"Campaign", &"can_save")
	describe(_context, _kind, can_suspend == true, can_save is int and can_save == SAVE_ALLOWED)


func default_focus() -> Control:
	return _resume


func back_requested() -> void:
	_on_resume()


## The key or button that opened the menu (Start on a pad) closes it too, while it is on top.
func _unhandled_input(event: InputEvent) -> void:
	if Screens.top() != self or event.is_echo() or not InputMap.has_action(&"menu"):
		return
	if event.is_action_pressed(&"menu") and not event.is_action_pressed(&"ui_cancel"):
		get_viewport().set_input_as_handled()
		back_requested()


func _detect_context() -> StringName:
	if Kit.call_on(&"Editor", &"in_splash") == true:
		return &"splash"
	var state: Variant = Kit.call_on(&"Session", &"state")
	if state is int and state == SESSION_REPLAY:
		return &"replay"
	if state is int and state == SESSION_IN_SORTIE:
		return &"sortie"
	if Kit.call_on(&"Campaign", &"is_active") == true:
		return &"chart"
	return &"sortie"


## Stop what can be stopped under the menu.
func _hold() -> void:
	match _context:
		&"sortie", &"splash":
			_paused_session = Kit.call_on(&"Session", &"set_paused", [true]) == true
		&"chart":
			if Kit.call_on(&"Campaign", &"is_advancing") == true:
				Kit.call_on(&"Campaign", &"set_clock_paused", [true])
				_paused_clock = true
		&"replay":
			var replay := Kit.as_dict(Kit.call_on(&"Session", &"replay_info"))
			if not replay.is_empty() and not Kit.get_bool(replay, "paused"):
				Kit.call_on(&"Session", &"replay_set_paused", [true])
				_paused_replay = true


func _restore() -> void:
	if _paused_session:
		Kit.call_on(&"Session", &"set_paused", [false])
		_paused_session = false
	if _paused_clock:
		Kit.call_on(&"Campaign", &"set_clock_paused", [false])
		_paused_clock = false
	if _paused_replay:
		Kit.call_on(&"Session", &"replay_set_paused", [false])
		_paused_replay = false


## Word the menu for its context (also the test entry point). `kind` is `Bp.KIND_*`.
func describe(context: StringName, kind: int, can_suspend: bool, can_save: bool) -> void:
	_context = context
	_kind = kind
	var paused := _paused_session or _paused_clock or _paused_replay
	_title.text = tr(&"pause.title.paused") if paused else tr(&"pause.title.menu")
	match context:
		&"splash":
			_state.text = tr(&"pause.state.splash") if paused else ""
			_set_leave(&"splash", &"pause.leave.splash", &"pause.note.splash")
		&"chart":
			_state.text = tr(&"pause.state.clock") if _paused_clock else ""
			if can_save:
				_set_leave(&"chart_save", &"pause.leave.chart_save", &"pause.note.chart_save")
			else:
				_set_leave(&"chart_lose", &"pause.leave.chart", &"pause.note.chart_lose")
		&"replay":
			_state.text = tr(&"pause.state.replay") if _paused_replay else ""
			_set_leave(&"replay", &"pause.leave.replay", &"")
		_:
			_state.text = tr(&"pause.state.paused") if paused else tr(&"pause.state.running")
			if kind == KIND_CAMPAIGN and can_suspend:
				_set_leave(&"suspend", &"pause.leave.suspend", &"pause.note.suspend")
			elif kind == KIND_CAMPAIGN:
				_set_leave(&"drop", &"pause.leave.operation", &"pause.note.drop")
			elif kind == KIND_SKIRMISH:
				_set_leave(&"lose", &"pause.leave.skirmish", &"pause.note.skirmish")
			elif kind == KIND_PVP:
				_set_leave(&"lose", &"pause.leave.match", &"pause.note.match")
			elif kind == KIND_TUTORIAL:
				_set_leave(&"tutorial", &"pause.leave.tutorial", &"pause.note.tutorial")
			elif kind == KIND_REPLAY:
				_set_leave(&"replay", &"pause.leave.replay", &"")
			else:
				_set_leave(&"plain", &"pause.leave.sortie", &"pause.note.sandbox" if kind == KIND_SANDBOX else &"")
	_state.visible = not _state.text.is_empty()
	_show_withdraw(context == &"sortie" and kind == KIND_CAMPAIGN)
	_panel.accessibility_name = _title.text
	_panel.accessibility_description = _state.text


func _show_withdraw(offered: bool) -> void:
	var ordered := offered and _seed >= 0 and _seed == _withdrawn_seed
	_withdraw.visible = offered
	_withdraw.disabled = ordered
	_withdraw_note.visible = offered
	_withdraw_note.text = tr(&"pause.note.withdrawing" if ordered else &"pause.note.withdraw") if offered else ""
	_withdraw.accessibility_description = _withdraw_note.text
	_chain()


func _chain() -> void:
	var order: Array = [_resume, _settings]
	if _withdraw.visible and not _withdraw.disabled:
		order.append(_withdraw)
	order.append(_leave)
	Kit.chain_vertical(order)
	Kit.link_sides(order, null, null)


func _set_leave(kind: StringName, label: StringName, note: StringName) -> void:
	_leave_kind = kind
	_leave.text = String(label)
	_leave_note.text = tr(note) if note != &"" else ""
	_leave_note.visible = not _leave_note.text.is_empty()
	_leave.accessibility_description = _leave_note.text


## What Leave will do, for tests.
func leave_kind() -> StringName:
	return _leave_kind


# --- actions -------------------------------------------------------------------------------------

func _on_resume() -> void:
	_restore()
	if Screens.top() == self:
		Screens.pop()


## Order the hull out and go back to the sortie: withdrawal happens in sortie time.
func _on_withdraw() -> void:
	if Kit.call_on(&"Orders", &"withdraw") != true:
		return
	_withdrawn_seed = _seed
	_on_resume()


func _on_leave() -> void:
	match _leave_kind:
		&"drop", &"lose", &"chart_lose":
			_ask_leave()
		_:
			_do_leave()


func _ask_leave() -> void:
	var title: StringName = &"pause.ask.title"
	var body: StringName = {&"drop": &"pause.note.drop", &"lose": &"pause.ask.lose", &"chart_lose": &"pause.note.chart_lose"}.get(_leave_kind, &"")
	var dialog := Screens.open_dialog(title, body, [
		{"id": &"leave", "label": StringName(_leave.text)},
		{"id": &"stay", "label": &"pause.ask.stay"},
	], &"stay", &"stay")
	if dialog != null:
		dialog.connect(&"chosen", func(id: StringName) -> void: _on_leave_chosen(id), CONNECT_ONE_SHOT)


func _on_leave_chosen(id: StringName) -> void:
	if id == &"leave":
		_do_leave()


func _do_leave() -> void:
	_leaving = true
	match _leave_kind:
		&"splash":
			_restore()
			Kit.call_on(&"Editor", &"splash_end")
			if Screens.top() == self:
				Screens.pop()
			return
		&"chart_save", &"chart_lose":
			_restore()
			Kit.call_on(&"Campaign", &"close_campaign")
		&"tutorial":
			Kit.call_on(&"Tutorial", &"stop")
			Kit.call_on(&"Session", &"leave", [false])
		_:
			# A campaign sortie writes the suspend save inside `leave` (L112) before going idle.
			Kit.call_on(&"Session", &"leave", [false])
			if _kind == KIND_CAMPAIGN and Kit.call_on(&"Campaign", &"is_active") == true:
				Kit.call_on(&"Campaign", &"close_campaign")
	# The router moves to the main menu when the session goes idle outside a campaign; a campaign
	# closed from a sortie leaves a base the router does not own, so go there directly.
	var base := Screens.base_name()
	if base in Screens.SESSION_BASES or base in Screens.CAMPAIGN_BASES or Screens.top() == self:
		Screens.change(&"main_menu")


func _apply_scrim() -> void:
	_scrim.color = Ui.scrim_colour()


func _on_ui_changed(what: StringName) -> void:
	if what == &"transparency" or what == &"palette":
		_apply_scrim()
