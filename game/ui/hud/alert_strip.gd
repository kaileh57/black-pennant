extends PanelContainer
## Alert strip: 5 alerts plus "+N", Acknowledge, Silence; live region.
## DESIGN.md L1676-1687. Unit: ui-hud-alerts.
## Reads and calls: Hud.alert_strip, Hud.alerts_changed, Orders.acknowledge, Orders.silence_alerts.
##
## The strip sits over the 3D view at the top of the centre column (HUD rule 18: opaque backing at
## `access.hud_backing_opacity`, forced opaque under reduce transparency; the text stays at full
## strength because only the backing fades).
## - Calm (no alert on the strip, no silence, no flood): the strip is hidden, so the calm state
##   draws zero alert-colour pixels (rule 1).
## - Up to five rows in the strip's order (highest priority, then the oldest). Each row is an
##   `AlertBadge` (the priority's own glyph, word and fill, with the blink rules and the Cleared
##   state of L1687) and the alert's sentence. Clicking the sentence goes to what the alert is
##   about: a track centres the plot on it, a compartment or part opens damage control.
## - "+N more" under the rows opens the log on its alert entries (the log keeps all times). Where
##   the centre is too short for five rows over the helm cluster (a large UI scale), the HUD shell
##   lowers the row count (`set_max_rows`, never under one) and the rest count into "+N" (floor 2).
## - Flood mode (more than 10 alerts in 10 minutes) adds one line saying what the strip is doing.
## - Acknowledge takes the alert the simulation will take (`next_ack`: highest, then oldest; there
##   is no acknowledge-all) and names it in its tooltip; it is disabled when nothing needs it.
##   Silence mutes alert sounds for 30 s and the strip counts the silence down at 1 Hz.
##   Both show their shortcut (Z and X by default) on the device in use.
## - The strip is an accessibility live region (L1743): polite, assertive while an
##   unacknowledged Alarm or Emergency is on it. Its description is the strip read in order.
## `show_strip(strip)` draws a hand-built `AlertStripDict` (tests and captures).

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const BadgeScene := preload("res://ui/widgets/alert_badge.tscn")
const HintScene := preload("res://ui/widgets/shortcut_hint.tscn")

## The strip shows at most this many alerts before "+N" (L1687).
const SHOWN := 5
## Width of an alert's sentence: up to 420 canvas units, less when the space between the panels
## beside the strip is narrower (large UI scale), never under 160. Longer sentences end in an
## ellipsis and read in full in the tooltip and the accessible name.
const TEXT_W := 420.0
const TEXT_MIN_W := 96.0
const LIVE_POLITE := 1
const LIVE_ASSERTIVE := 2
## `Bp.*` values used before the extension loads (the compile check instances every scene).
const NONE := -1
const PRIORITY_ALARM := 2
const ALERT_UNACKNOWLEDGED := 0
const LOG_ALERT := 5

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column
@onready var _rows_box: VBoxContainer = $Margin/Column/Rows
@onready var _more: Button = $Margin/Column/More
@onready var _flood: Label = $Margin/Column/Flood
@onready var _actions: HBoxContainer = $Margin/Column/Actions
@onready var _ack: Button = $Margin/Column/Actions/Acknowledge
@onready var _silence: Button = $Margin/Column/Actions/Silence
@onready var _silenced: Label = $Margin/Column/Actions/Silenced

var _rows: Array[HBoxContainer] = []
var _badges: Array[Control] = []
var _texts: Array[Button] = []
var _shown: Array[Dictionary] = []
var _strip: Dictionary = {}
var _ack_hint: Control
var _silence_hint: Control
var _silence_timer: Timer
var _live := false
var _summary := ""
## Rows the HUD shell leaves room for, 1..SHOWN.
var _max_rows := SHOWN
## Width the HUD shell leaves the strip between the docks (`set_max_width`); -1: measure the centre.
var _max_width := -1.0
## Acknowledge and Silence as icons with their keys, for a narrow centre.
var _compact_actions := false


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_margins(_margin, 2)
	Ui.set_separation(_column, 1)
	Ui.set_separation(_rows_box, 1)
	Ui.set_separation(_actions, 2)
	for i in SHOWN:
		_add_row(i)
	_more.focus_mode = Control.FOCUS_ALL
	_more.pressed.connect(_open_log)
	_flood.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_silenced.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_more.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_ack.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_silence.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_ack.focus_mode = Control.FOCUS_ALL
	_silence.focus_mode = Control.FOCUS_ALL
	_ack.icon = Ui.icon(&"check_pass")
	_silence.icon = Ui.icon(&"alert_silence")
	_ack.pressed.connect(_on_acknowledge)
	_silence.pressed.connect(_on_silence)
	_ack_hint = _hint(&"alert_acknowledge", _ack)
	_silence_hint = _hint(&"alert_silence", _silence)
	_silence_timer = Timer.new()
	_silence_timer.wait_time = 1.0
	_silence_timer.timeout.connect(_tick_silence)
	add_child(_silence_timer)
	Ui.changed.connect(_on_ui_changed)
	_apply_backing()
	var shell := _hud()
	var centre := shell.get_node_or_null(^"Frame/Docks/Centre") as Control if shell != null else null
	if centre != null:
		centre.resized.connect(_fit_width)
	elif get_parent() is Control:
		(get_parent() as Control).resized.connect(_fit_width)
	var hud := Kit.autoload(&"Hud")
	if hud != null:
		_live = true
		if hud.has_signal(&"alerts_changed"):
			hud.connect(&"alerts_changed", refresh)
		if hud.has_signal(&"ship_static_changed"):
			hud.connect(&"ship_static_changed", refresh)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: refresh())
	_render_static_text()
	refresh()


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render_static_text()
		show_strip(_strip)


func _add_row(i: int) -> void:
	var row := HBoxContainer.new()
	row.name = "Row%d" % i
	row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_separation(row, 2)
	var badge: Control = BadgeScene.instantiate()
	badge.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(badge)
	var text := Button.new()
	text.theme_type_variation = &"FlatButton"
	text.alignment = HORIZONTAL_ALIGNMENT_LEFT
	text.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	text.custom_minimum_size = Vector2(TEXT_W, 32.0)
	text.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	text.focus_mode = Control.FOCUS_ALL
	text.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	text.pressed.connect(_on_row_pressed.bind(i))
	row.add_child(text)
	row.visible = false
	_rows_box.add_child(row)
	_rows.append(row)
	_badges.append(badge)
	_texts.append(text)


func _hint(action: StringName, after: Control) -> Control:
	var hint: Control = HintScene.instantiate()
	hint.set_meta(&"action", action)
	hint.set(&"compact", true)
	hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	hint.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_actions.add_child(hint)
	_actions.move_child(hint, after.get_index() + 1)
	return hint


func _render_static_text() -> void:
	# In a narrow centre (a large UI scale) the two buttons keep their icons and keys only; their
	# names stay in the tooltips and accessible names (floor 2: abbreviation allowed at 200%).
	_ack.text = "" if _compact_actions else tr(&"hud_alerts.strip.acknowledge")
	_silence.text = "" if _compact_actions else tr(&"hud_alerts.strip.silence")
	_silence.tooltip_text = tr(&"hud_alerts.strip.silence_tip")
	_silence.accessibility_name = tr(&"hud_alerts.strip.silence_tip")
	accessibility_name = tr(&"hud_alerts.strip.access")


# --- data ----------------------------------------------------------------------------------------

## Pull the strip from Hud and draw it.
func refresh() -> void:
	if not is_node_ready():
		return
	var strip: Dictionary = {}
	if _live:
		strip = Kit.as_dict(Kit.call_on(&"Hud", &"alert_strip"))
	show_strip(strip)


## Show at most `rows` alerts (1 to 5); the rest count into "+N". The HUD shell sets it from the room
## the centre leaves above the helm cluster.
func set_max_rows(rows: int) -> void:
	var n := clampi(rows, 1, SHOWN)
	if n == _max_rows:
		return
	_max_rows = n
	if is_node_ready():
		show_strip(_strip)


## The height of one alert row and of everything around the rows, for the HUD shell's sums.
func row_height() -> float:
	return _rows[0].get_combined_minimum_size().y + float(_rows_box.get_theme_constant(&"separation")) if not _rows.is_empty() else 40.0


## Draw one `AlertStripDict`. An empty Dictionary is the calm state.
func show_strip(strip: Dictionary) -> void:
	if not is_node_ready():
		return
	_strip = strip
	_shown = Kit.dicts(strip.get("shown", []))
	var more := maxi(0, Kit.get_int(strip, "more", 0))
	if _shown.size() > _max_rows:
		more += _shown.size() - _max_rows
		_shown.resize(_max_rows)
	var flood := Kit.get_bool(strip, "flood")
	var silenced := maxf(0.0, Kit.get_float(strip, "silenced_s", 0.0))
	var next_ack := Kit.get_int(strip, "next_ack", NONE)
	var urgent := false
	var lines: PackedStringArray = []
	for i in SHOWN:
		var on := i < _shown.size()
		_rows[i].visible = on
		if not on:
			continue
		var a := _shown[i]
		_badges[i].call(&"show_alert", a)
		var sentence := _Text.resolve(a.get("text", {}))
		_texts[i].text = sentence
		var word := _priority_word(a)
		_texts[i].tooltip_text = sentence
		_texts[i].accessibility_name = tr(&"hud_alerts.strip.row_access").format([word, sentence])
		lines.append(tr(&"hud_alerts.strip.row_access").format([word, sentence]))
		if Kit.get_int(a, "priority", 0) >= PRIORITY_ALARM and Kit.get_int(a, "state", ALERT_UNACKNOWLEDGED) == ALERT_UNACKNOWLEDGED:
			urgent = true
	_align_badges()
	_fit_width()
	_more.visible = more > 0
	if more > 0:
		_more.text = tr(&"hud_alerts.strip.more").format([_count(more)])
		_more.tooltip_text = tr(&"hud_alerts.strip.more_tip")
		lines.append(_more.text)
	_flood.visible = flood
	if flood:
		_flood.text = tr(&"hud_alerts.strip.flood")
		lines.append(_flood.text)
	# Acknowledge names the alert it will take.
	_ack.disabled = next_ack == NONE
	var target := _find(next_ack)
	if target.is_empty():
		_ack.tooltip_text = tr(&"hud_alerts.strip.acknowledge_none")
	else:
		_ack.tooltip_text = tr(&"hud_alerts.strip.acknowledge_tip").format([_Text.resolve(target.get("text", {}))])
	_ack.accessibility_name = _ack.tooltip_text
	_show_silence(silenced)
	if silenced > 0.0:
		lines.append(_silenced.text)
	var any := not _shown.is_empty() or more > 0 or flood or silenced > 0.0
	visible = any
	if any:
		# Key names are read once the strip first shows (no keyboard exists in a headless check).
		for hint: Control in [_ack_hint, _silence_hint]:
			if hint.get(&"action") != hint.get_meta(&"action"):
				hint.set(&"action", hint.get_meta(&"action"))
	_ack.visible = not _shown.is_empty()
	_ack_hint.visible = _ack.visible
	set(&"accessibility_live", LIVE_ASSERTIVE if urgent else LIVE_POLITE)
	var summary := "\n".join(lines)
	if summary != _summary:
		_summary = summary
		accessibility_description = summary
	update_minimum_size()


## Every badge as wide as the widest shown, so the sentences start in one column.
func _align_badges() -> void:
	var w := 0.0
	for i in _shown.size():
		_badges[i].custom_minimum_size.x = 0.0
		w = maxf(w, _badges[i].get_combined_minimum_size().x)
	for i in _shown.size():
		_badges[i].custom_minimum_size.x = w


func _show_silence(left_s: float) -> void:
	var on := left_s > 0.0
	_silenced.visible = on
	if on:
		_silenced.text = tr(&"hud_alerts.strip.silenced").format([_duration(ceilf(left_s))])
		if _silence_timer.is_stopped():
			_silence_timer.start()
	elif not _silence_timer.is_stopped():
		_silence_timer.stop()


## The countdown is not a change signal, so it is read again once a second while it runs.
func _tick_silence() -> void:
	if not _live:
		_silence_timer.stop()
		return
	var strip := Kit.as_dict(Kit.call_on(&"Hud", &"alert_strip"))
	var left := maxf(0.0, Kit.get_float(strip, "silenced_s", 0.0))
	if left <= 0.0 or not visible:
		show_strip(strip)
		return
	_show_silence(left)


func _find(id: int) -> Dictionary:
	if id == NONE:
		return {}
	for a: Dictionary in _shown:
		if Kit.get_int(a, "id", NONE) == id:
			return a
	var one: Variant = Kit.call_on(&"Hud", &"alert", [id]) if _live else null
	return one if one is Dictionary else {}


func _priority_word(a: Dictionary) -> String:
	if Kit.get_int(a, "state", 0) == 2:
		return tr(&"widgets.alert.cleared")
	var w := StringName(a.get("word", &""))
	return tr(w) if w != &"" else ""


func _count(n: int) -> String:
	return TranslationServer.format_number(str(n), TranslationServer.get_locale())


func _duration(seconds: float) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"duration"):
		return String(tx.call(&"duration", seconds))
	return "%d s" % int(seconds)


# --- actions -------------------------------------------------------------------------------------

func _on_acknowledge() -> void:
	Kit.call_on(&"Orders", &"acknowledge", [NONE])


func _on_silence() -> void:
	Kit.call_on(&"Orders", &"silence_alerts")


## Go to what an alert is about: a track on the plot, a compartment or part in damage control.
func _on_row_pressed(i: int) -> void:
	if i >= _shown.size():
		return
	var a := _shown[i]
	var hud := _hud()
	if hud == null:
		return
	var track := Kit.get_int(a, "track", NONE)
	if track != NONE and hud.has_method(&"centre_on_log"):
		hud.call(&"centre_on_log", {"track": track, "pos": null})
		return
	if (Kit.get_int(a, "compartment", NONE) != NONE or Kit.get_int(a, "part", NONE) != NONE) and hud.has_method(&"open_station"):
		if not _station_open(hud, &"damage"):
			hud.call(&"open_station", &"damage")


## Controls a keyboard or gamepad player reaches in this strip, in order (the sortie HUD links
## them into its panel focus chain): Acknowledge and Silence while shown.
func focus_targets() -> Array:
	var out: Array = []
	for b: Button in [_ack, _silence, _more]:
		if b.is_visible_in_tree() and b.focus_mode != Control.FOCUS_NONE:
			out.append(b)
	return out


func _open_log() -> void:
	var hud := _hud()
	if hud != null and hud.has_method(&"open_station") and not _station_open(hud, &"log"):
		hud.call(&"open_station", &"log")
	get_tree().call_group(&"hud_log_panel", &"show_kind", LOG_ALERT)


static func _station_open(hud: Node, station: StringName) -> bool:
	return hud.has_method(&"station") and StringName(hud.call(&"station")) == station


## The sortie HUD this strip sits in (the nearest ancestor that can centre the plot).
func _hud() -> Node:
	var n := get_parent()
	while n != null:
		if n.has_method(&"centre_on_log") and n.has_method(&"open_station"):
			return n
		n = n.get_parent()
	return null


# --- look ----------------------------------------------------------------------------------------

## Fit the sentence column to the room the strip has. The strip holds its parent open, so the room
## is read from the sortie view between the docks less the panels beside the strip in its row.
## The Acknowledge and Silence row's width with their names (`compact` false) or without.
func _actions_width(compact: bool) -> float:
	var w := 0.0
	var shown := 0
	for c: Node in _actions.get_children():
		var ctl := c as Control
		if ctl == null or not ctl.visible or ctl == _silenced:
			continue
		shown += 1
		if ctl is Button:
			var b := ctl as Button
			var text := "" if compact else (tr(&"hud_alerts.strip.acknowledge") if b == _ack else tr(&"hud_alerts.strip.silence"))
			var font := b.get_theme_font(&"font")
			var fs := b.get_theme_font_size(&"font_size")
			var style := b.get_theme_stylebox(&"normal")
			var icon_w := float(b.icon.get_width()) if b.icon != null else 0.0
			w += icon_w + (font.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, fs).x + float(b.get_theme_constant(&"h_separation")) if not text.is_empty() else 0.0)
			w += style.get_margin(SIDE_LEFT) + style.get_margin(SIDE_RIGHT) if style != null else 0.0
		else:
			w += ctl.get_combined_minimum_size().x
	return w + float(_actions.get_theme_constant(&"separation")) * maxf(0.0, shown - 1)


## The width the strip may take, from the HUD shell's arithmetic (window less the docks), so the
## strip never widens the centre it is measured in.
func set_max_width(width: float) -> void:
	if is_equal_approx(width, _max_width):
		return
	_max_width = width
	if is_node_ready():
		_fit_width()


func _fit_width() -> void:
	var parent := get_parent() as Control
	if parent == null:
		return
	if _max_width >= 0.0:
		_fit_texts(_max_width)
		return
	var room := parent.size.x
	var row := parent.get_parent() as BoxContainer
	var hud := _hud()
	var centre := hud.get_node_or_null(^"Frame/Docks/Centre/Margin") as Control if hud != null else null
	if centre != null and row != null:
		var margins := float(centre.get_theme_constant(&"margin_left") + centre.get_theme_constant(&"margin_right"))
		room = centre.size.x - margins
		var shown := 0
		for c: Node in row.get_children():
			if c is Control and (c as Control).visible:
				shown += 1
				if c != parent:
					room -= (c as Control).get_combined_minimum_size().x
		room -= float(row.get_theme_constant(&"separation")) * maxf(0.0, shown - 1)
	_fit_texts(room)


func _fit_texts(room: float) -> void:
	var compact := false
	if _max_width >= 0.0:
		var full := float(Ui.spacing(2) * 2) + _actions_width(false)
		var panel_box := get_theme_stylebox(&"panel")
		if panel_box != null:
			full += panel_box.get_margin(SIDE_LEFT) + panel_box.get_margin(SIDE_RIGHT)
		compact = full > room
	if compact != _compact_actions:
		_compact_actions = compact
		_render_static_text()
	var badge := 0.0
	for b: Control in _badges:
		badge = maxf(badge, b.get_combined_minimum_size().x)
	# The panel's own margin, the strip's margin each side and the gap after the badge.
	var panel := get_theme_stylebox(&"panel")
	var frame := panel.get_margin(SIDE_LEFT) + panel.get_margin(SIDE_RIGHT) if panel != null else 0.0
	var row_gap := float(_rows[0].get_theme_constant(&"separation")) if not _rows.is_empty() else float(Ui.spacing(1))
	var chrome := float(_margin.get_theme_constant(&"margin_left") + _margin.get_theme_constant(&"margin_right")) + row_gap + badge + frame
	var w := clampf(room - chrome, TEXT_MIN_W, TEXT_W)
	for t: Button in _texts:
		if not is_equal_approx(t.custom_minimum_size.x, w):
			t.custom_minimum_size.x = w

func _on_ui_changed(what: StringName) -> void:
	if what == &"backing" or what == &"transparency":
		_apply_backing()


## Only the backing fades; the text and badges stay at full strength (rule 18).
func _apply_backing() -> void:
	self_modulate.a = 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)
