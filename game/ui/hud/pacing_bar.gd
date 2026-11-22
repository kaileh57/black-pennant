extends PanelContainer
## Pause, compression rate, time scale, ladder rung, host tick rate.
## DESIGN.md L110-116, L1019-1029. Unit: ui-hud-ship.
## Reads and calls: Session.pacing, Session.pacing_changed, Session.set_paused, Session.request_time_rate.
##
## One row: Pause (single-player only), the three compression rates 1x, 2x and 4x as toggles that
## show the rate this player asked for, and at its end the effective rate, which the HUD always
## shows (L116).
## Under it, only when they apply (rule 1, quiet when normal):
## - "Held at 1x: torpedo inbound." while this player asks for more than the effective rate, with
##   the reason the server gives (L113-114). In co-op it names who asks for less.
## - The overload time scale whenever it is below 1x, boxed for 10 s after each change, because
##   silent slow motion is the complaint L1022 answers.
## - The host's ladder rung in words while the host has stepped down.
## - The host's tick time (p95, ms) in multiplayer, and in single-player while the ladder is
##   stepped down (L1022: every player sees the host's tick rate); setting `hud.show_host_tick`.
## PvP has neither pause nor compression (L115): the rate buttons and Pause hide. The keyboard
## (= and -, P) is the HUD shell's; these buttons are for the pointer and for gamepad focus.
## `show_pacing(pacing, info)` renders plain data (tests and captures); `refresh()` pulls it.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

const RATES: Array[int] = [1, 2, 4]
## Ladder texts by rung and sub-step (L1019-1023). Rung 1 on the host is the compression ceiling.
const RUNG_KEYS: Dictionary[int, Array] = {
	1: [&"hud_ship.rung.compression"],
	2: [&"hud_ship.rung.kinematic_far", &"hud_ship.rung.kinematic_all"],
	3: [&"hud_ship.rung.projectiles_1500", &"hud_ship.rung.projectiles_1000"],
	4: [&"hud_ship.rung.time_scale"],
}

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column
@onready var _row: HFlowContainer = $Margin/Column/Row
@onready var _pause: Button = $Margin/Column/Row/Pause
@onready var _rates_box: HBoxContainer = $Margin/Column/Row/Rates
@onready var _rate_buttons: Array[Button] = [$Margin/Column/Row/Rates/Rate1, $Margin/Column/Row/Rates/Rate2, $Margin/Column/Row/Rates/Rate4]
@onready var _effective: Label = $Margin/Column/Row/Effective
@onready var _held: Label = $Margin/Column/Held
@onready var _scale: Label = $Margin/Column/Scale
@onready var _rung: Label = $Margin/Column/Rung
@onready var _tick: Label = $Margin/Column/Tick

var _pacing: Dictionary = {}
var _info: Dictionary = {}
var _last_scale := 1.0
var _box: ModeBox
var _box_until_ms := 0
var _asked_rate := 1
var _asked_until_ms := 0


func _ready() -> void:
	# Docked: the panel's own 16 px margin is the dock's inset (every docked cluster keeps 16 px,
	# so their text lines up down the dock).
	Ui.set_margins(_margin, 1)
	for side: StringName in [&"margin_left", &"margin_right", &"margin_top", &"margin_bottom"]:
		_margin.add_theme_constant_override(side, 0)
	Ui.set_separation(_column, 0)
	# A flow: in a narrow dock (large UI scale) the effective rate moves under the buttons.
	_row.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	_row.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	Ui.set_separation(_rates_box, 0)
	for l: Label in [_effective, _held, _scale, _rung, _tick]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_pause.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_pause.focus_mode = Control.FOCUS_ALL
	_pause.pressed.connect(_on_pause)
	for i in RATES.size():
		var b := _rate_buttons[i]
		b.focus_mode = Control.FOCUS_ALL
		b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		b.pressed.connect(_on_rate.bind(RATES[i]))
	Kit.chain_horizontal([_pause] + _rate_buttons)
	_box = ModeBox.new()
	_box.visible = false
	_scale.add_child(_box)
	accessibility_name = tr(&"hud_ship.pacing.access")
	_texts()
	_on(&"Session", &"pacing_changed", refresh)
	_on(&"Session", &"state_changed", func(_s: int) -> void: refresh())
	_on(&"Settings", &"changed", _on_setting)
	if Ui.has_signal(&"changed"):
		Ui.changed.connect(_on_ui_changed)
	_apply_backing()
	refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_texts()
		show_pacing(_pacing, _info)


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _texts() -> void:
	for i in RATES.size():
		var b := _rate_buttons[i]
		b.text = rate_text(float(RATES[i]), 0)
		b.tooltip_text = tr(&"hud_ship.rate.tooltip").format([b.text]) if RATES[i] > 1 else tr(&"hud_ship.rate.tooltip_1")
		b.accessibility_name = tr(&"hud_ship.rate.access").format([b.text])


func _on_setting(key: StringName) -> void:
	if key == &"hud.show_host_tick" or key == &"":
		show_pacing(_pacing, _info)


func _on_ui_changed(what: StringName) -> void:
	if what == &"backing" or what == &"transparency":
		_apply_backing()


## The panel sits over the 3D view: its own background takes the HUD backing opacity, its text
## stays at full contrast (rule 18).
func _apply_backing() -> void:
	self_modulate.a = 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)


# --- data ----------------------------------------------------------------------------------------

func refresh() -> void:
	if not is_node_ready():
		return
	show_pacing(Kit.as_dict(Kit.call_on(&"Session", &"pacing")), Kit.as_dict(Kit.call_on(&"Session", &"info")))


## Render a `PacingDict` with the `SessionDict` it belongs to. Also the test entry point.
func show_pacing(pacing: Dictionary, info: Dictionary) -> void:
	_pacing = pacing
	_info = info
	if not is_node_ready():
		return
	var state := Kit.get_int(info, "state", Bp.SESSION_IDLE)
	var kind := Kit.get_int(info, "kind", Bp.KIND_NONE)
	var net := Kit.get_int(info, "net", Bp.NET_OFFLINE)
	var in_sortie := state == Bp.SESSION_IN_SORTIE
	var pvp := kind == Bp.KIND_PVP
	var replay := kind == Bp.KIND_REPLAY or state == Bp.SESSION_REPLAY
	var blocked := Kit.get_int(pacing, "blocked", Bp.NONE)
	var paused := Kit.get_bool(pacing, "paused")
	var effective := _rate_of(Kit.get_int(pacing, "rate", 1))
	var mine := requested_by(pacing, Kit.get_int(info, "player", Bp.NONE), effective)
	# Pause: single-player only (L111, L114-115).
	_pause.visible = Kit.get_bool(pacing, "can_pause") and not replay
	_pause.text = tr(&"hud_ship.pause.resume" if paused else &"hud_ship.pause.pause")
	_pause.tooltip_text = tr(&"hud_ship.pause.tooltip_resume" if paused else &"hud_ship.pause.tooltip")
	# Compression: never in PvP or where the mode forbids it.
	var compress := in_sortie and not pvp and not replay and blocked != Bp.BLOCK_MODE_FORBIDS
	_rates_box.visible = compress
	for i in RATES.size():
		_rate_buttons[i].set_pressed_no_signal(RATES[i] == mine)
		_rate_buttons[i].disabled = paused
	_effective.text = tr(&"hud_ship.rate.effective_paused" if paused else &"hud_ship.rate.effective").format([rate_text(float(effective), 0)])
	_effective.visible = in_sortie or replay
	# Held: the player asks for more than they get.
	# A request the server dropped straight back to the effective rate still explains itself for
	# the mode-change time after the press.
	var asked := mine
	if Time.get_ticks_msec() < _asked_until_ms:
		asked = maxi(asked, _asked_rate)
	var held := held_text(pacing, info, asked, effective)
	_held.text = held
	_held.visible = compress and not paused and not held.is_empty()
	# Time scale below 1x (rung 4).
	var scale_value := Kit.get_float(pacing, "time_scale", 1.0)
	if not is_finite(scale_value):
		scale_value = 1.0
	var degraded := scale_value < 0.995
	_scale.visible = in_sortie and degraded
	_scale.text = tr(&"hud_ship.scale.text").format([rate_text(scale_value, 1)])
	if in_sortie and not is_equal_approx(scale_value, _last_scale):
		_box_until_ms = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0)
	_last_scale = scale_value
	_box.visible = _scale.visible and Time.get_ticks_msec() < _box_until_ms
	set_process(_box.visible or Time.get_ticks_msec() < _asked_until_ms)
	# Ladder rung.
	var rung := Kit.get_int(pacing, "rung", 0)
	var rung_line := rung_text(rung, Kit.get_int(pacing, "sub_step", 0))
	_rung.visible = in_sortie and not rung_line.is_empty()
	_rung.text = rung_line
	# Host tick.
	var tick_ms := Kit.get_float(pacing, "host_tick_ms", 0.0)
	var show_tick := in_sortie and _show_tick_setting() and tick_ms > 0.0 and (net != Bp.NET_OFFLINE or rung > 0)
	_tick.visible = show_tick
	_tick.text = tr(&"hud_ship.tick.text").format([Kit.number(tick_ms, &"none", 1)])
	visible = in_sortie or replay
	_access(held)


func _show_tick_setting() -> bool:
	var v: Variant = Kit.call_on(&"Settings", &"get_value", [&"hud.show_host_tick"])
	if v == null:
		return true
	return v == true or (v is int and int(v) != 0)


func _access(held: String) -> void:
	var parts: PackedStringArray = [_effective.text]
	if not held.is_empty() and _held.visible:
		parts.append(held)
	if _scale.visible:
		parts.append(_scale.text)
	if _rung.visible:
		parts.append(_rung.text)
	accessibility_description = " ".join(parts)


func _process(_delta: float) -> void:
	var now := Time.get_ticks_msec()
	if _box.visible and now >= _box_until_ms:
		_box.visible = false
	if _asked_until_ms > 0 and now >= _asked_until_ms:
		_asked_until_ms = 0
		show_pacing(_pacing, _info)
	if not _box.visible and _asked_until_ms == 0:
		set_process(false)


# --- pure rules (static so tests share them) -----------------------------------------------------

static func _rate_of(v: int) -> int:
	return v if v in RATES else 1


## The rate `player` asks for, from `PacingDict.requested`; the effective rate when the list does
## not name the player (single-player hosts may not list it).
static func requested_by(pacing: Dictionary, player: int, effective: int) -> int:
	for r: Dictionary in Kit.dicts(pacing.get("requested", [])):
		if Kit.get_int(r, "player", Bp.NONE) == player:
			return _rate_of(Kit.get_int(r, "rate", effective))
	return effective


## The server's reason ("Time held at 1x: weapon inbound") while the player asks for more than
## the effective rate; "" otherwise.
## The reason is the server's; in co-op without one, the players asking for less are named.
static func held_text(pacing: Dictionary, info: Dictionary, mine: int, effective: int) -> String:
	if mine <= effective:
		return ""
	var reason := _Text.resolve(pacing.get("blocked_text", {}))
	var rate := rate_text(float(effective), 0)
	if reason.is_empty() or Kit.get_int(pacing, "blocked", Bp.NONE) == Bp.BLOCK_NOT_ALL_PLAYERS:
		var names := slower_players(pacing, Kit.get_int(info, "player", Bp.NONE), mine)
		if not names.is_empty():
			return TranslationServer.translate(&"hud_ship.held.players").format([rate, ", ".join(names)])
	if reason.is_empty():
		return TranslationServer.translate(&"hud_ship.held.plain").format([rate])
	# The server's sentence already names the rate ("Time held at 1x: weapon inbound").
	return reason


## Names of the other players whose request is below `mine` (co-op, L114).
static func slower_players(pacing: Dictionary, me: int, mine: int) -> PackedStringArray:
	var names: PackedStringArray = []
	var conns := Kit.dicts(Kit.call_on(&"Session", &"connections"))
	for r: Dictionary in Kit.dicts(pacing.get("requested", [])):
		var p := Kit.get_int(r, "player", Bp.NONE)
		if p == me or Kit.get_int(r, "rate", 1) >= mine:
			continue
		var name := ""
		for c: Dictionary in conns:
			if Kit.get_int(c, "player", -2) == p:
				name = Kit.get_str(c, "name")
		if name.is_empty():
			name = TranslationServer.translate(&"hud_ship.held.player_n").format([str(p + 1)])
		names.append(name)
	return names


## The ladder rung in words, or "" at rung 0 (L1019-1023).
static func rung_text(rung: int, sub_step: int) -> String:
	if not RUNG_KEYS.has(rung):
		return ""
	var keys: Array = RUNG_KEYS[rung]
	var key: StringName = keys[clampi(sub_step, 0, keys.size() - 1)]
	return TranslationServer.translate(&"hud_ship.rung.line").format([str(rung), TranslationServer.translate(key)])


## "2x", "0.8x".
static func rate_text(value: float, decimals: int) -> String:
	return TranslationServer.translate(&"hud_ship.rate.factor").format([Kit.number(value, &"none", decimals)])


## The 2 px ui.box frame of a change (L1649), over the time-scale line for the mode-change time.
class ModeBox extends Control:
	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_IGNORE
		set_anchors_preset(Control.PRESET_FULL_RECT)

	func _notification(what: int) -> void:
		if what == NOTIFICATION_THEME_CHANGED or what == NOTIFICATION_RESIZED:
			queue_redraw()

	func _draw() -> void:
		# Drawn just outside the line, so the frame never touches the text.
		var w := 2.0
		draw_rect(Rect2(Vector2(-4.0, -1.0), size + Vector2(8.0, 2.0)), Ui.colour(&"ui.box"), false, w)


func _on_pause() -> void:
	var paused := Kit.get_bool(Kit.as_dict(Kit.call_on(&"Session", &"pacing")), "paused")
	Kit.call_on(&"Session", &"set_paused", [not paused])


## The time keys (= and -, the D-pad) step the request through the HUD shell; it tells the bar, so a
## step the server answers with the same rate still says why it held (the buttons do the same).
func note_step(direction: int) -> void:
	if direction <= 0:
		_asked_until_ms = 0
		refresh()
		return
	var effective := _rate_of(Kit.get_int(_pacing, "rate", 1))
	var mine := requested_by(_pacing, Kit.get_int(_info, "player", Bp.NONE), effective)
	var i := RATES.find(mine)
	var next := RATES[mini(maxi(i, 0) + 1, RATES.size() - 1)]
	_asked_rate = next
	_asked_until_ms = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0) if next > 1 else 0
	refresh()


func _on_rate(rate: int) -> void:
	_asked_rate = rate
	_asked_until_ms = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0) if rate > 1 else 0
	Kit.call_on(&"Session", &"request_time_rate", [rate])
	# The toggle shows the request once the server answers; until then it keeps its state.
	refresh()
