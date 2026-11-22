extends HFlowContainer
## Theatre date-time group, operation clock, advance and pause.
## DESIGN.md L741, L750. Unit: ui-chart.
## Reads and calls: Campaign.advance, Campaign.set_clock_paused, Campaign.interrupted, Tx.date_time_group.
##
## Left to right: the date-time group and the day with its time of day; the act and the operation
## clock (days left, or Open Theatre); what theatre time is doing (running, held, or stopped and why,
## the interrupt sentence, L741) with the hunter-group warning under it when one is out (L749);
## Continue, Pause or Resume, and the animation speed.
##
## Time moves only on a committed decision, which starts it at once. Continue appears after an
## interrupt that stopped time part way through a transit or wait, so the player can carry on
## without deciding again; it never shows when nothing is pending, where `advance` would only report
## the wait done. Pause holds the animation only (not a simulation pause). There is no getter for the
## held state, so the bar keeps its own: an hour that passes while it thinks time is held means
## someone resumed it, and when the pause menu (which holds and releases the clock itself) closes,
## a hold the player set here is put back.
##
## The pending action, the hold and the speed live in static variables, so they survive the table
## being rebuilt when the player opens a briefing and comes back.

signal state_changed

const Kit := preload("res://ui/screens/menu_kit.gd")
## `Bp` values used here, kept so the script parses before the extension loads.
const PHASE_ADVANCING := 2
const PHASE_SORTIE := 5
const INTERRUPT_ARRIVED := 0
const INTERRUPT_REFIT_DONE := 1
const INTERRUPT_WAIT_DONE := 2
const INTERRUPT_FORCED_OPERATION := 4
const INTERRUPT_ACT_ENDED := 8
## Interrupts after which no time action is left to run.
const DONE_KINDS: Array[int] = [INTERRUPT_ARRIVED, INTERRUPT_REFIT_DONE, INTERRUPT_WAIT_DONE, INTERRUPT_FORCED_OPERATION, INTERRUPT_ACT_ENDED]
## Animation speeds offered, theatre hours per second (Campaign takes 1 to 48).
const SPEEDS: Array[float] = [3.0, 6.0, 12.0, 24.0, 48.0]
const DEFAULT_SPEED := 6.0

static var _pending := false
static var _held := false
static var _speed := DEFAULT_SPEED
static var _interrupt_text: Dictionary = {}
## The campaign the statics belong to; a different one clears them.
static var _campaign_key := ""

var _source: RefCounted
var _chart: Dictionary = {}

@onready var _dtg: Label = $When/Dtg
@onready var _day: Label = $When/Day
@onready var _act_label: Label = $Clock/Act
@onready var _days: Label = $Clock/Days
@onready var _state: Label = $Time/State
@onready var _hunter: Label = $Time/Hunter
@onready var _continue: Button = $Continue
@onready var _pause: Button = $Pause
@onready var _speed_button: OptionButton = $Speed


func _ready() -> void:
	add_theme_constant_override(&"h_separation", Ui.spacing(4))
	add_theme_constant_override(&"v_separation", Ui.spacing(1))
	Ui.set_separation($When, 0)
	Ui.set_separation($Clock, 0)
	Ui.set_separation($Time, 0)
	for l: Label in [_dtg, _day, _act_label, _days, _state, _hunter]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		l.text = ""
	_state.mouse_filter = Control.MOUSE_FILTER_PASS
	# Wide enough for an interrupt sentence; at a large UI scale it takes its own line in the flow.
	$Time.custom_minimum_size.x = 360.0
	_continue.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_pause.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_speed_button.custom_minimum_size = Vector2(0.0, Kit.BUTTON_H)
	_continue.tooltip_text = tr(&"chart.clock.continue_tip")
	_speed_button.tooltip_text = tr(&"chart.clock.speed_tip")
	_speed_button.accessibility_name = tr(&"chart.clock.speed_name")
	_speed_button.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for s: float in SPEEDS:
		_speed_button.add_item(tr(&"chart.clock.speed").format([Kit.number(s, &"h", 0)]))
	_speed_button.select(maxi(0, SPEEDS.find(_speed)))
	_continue.pressed.connect(_on_continue)
	_pause.pressed.connect(_on_pause)
	_speed_button.item_selected.connect(_on_speed)
	var screens := Kit.autoload(&"Screens")
	if screens != null and screens.has_signal(&"stack_changed"):
		screens.connect(&"stack_changed", _on_stack_changed)
	_render()


func bind(source: RefCounted) -> void:
	_source = source
	var key := Kit.get_str(Kit.as_dict(source.call(&"current_slot")), "campaign_id") if source != null else ""
	if key != _campaign_key:
		_campaign_key = key
		_pending = false
		_held = false
		_interrupt_text = {}
	if source != null:
		source.call(&"set_advance_speed", _speed)


## Re-read the chart (on `Campaign.chart_changed`).
func refresh() -> void:
	if _source == null:
		return
	_chart = _source.call(&"chart")
	_render()


## One theatre hour passed (on `Campaign.hour_advanced`): time is running, so it is not held.
func on_hour(_hour: int) -> void:
	_held = false
	_pending = false
	_interrupt_text = {}


## Theatre time stopped (on `Campaign.interrupted`).
func on_interrupted(interrupt: Dictionary) -> void:
	var kind := Kit.get_int(interrupt, "kind", -1)
	_pending = not DONE_KINDS.has(kind)
	_held = false
	_interrupt_text = Kit.as_dict(interrupt.get("text"))
	_render()


## A decision committed and started time: the old interrupt no longer applies.
func on_committed() -> void:
	_pending = false
	_interrupt_text = {}
	_render()


## Whether Continue is on offer (for tests and the table's focus order).
func can_continue() -> bool:
	return _continue.visible


func controls() -> Array[Control]:
	var out: Array[Control] = []
	for c: Control in [_continue, _pause, _speed_button]:
		if c.visible:
			out.append(c)
	return out


func _advancing() -> bool:
	return Kit.get_int(_chart, "phase", -1) == PHASE_ADVANCING or (_source != null and _source.call(&"is_advancing") == true)


func _render() -> void:
	if not is_node_ready():
		return
	if _chart.is_empty():
		_dtg.text = tr(&"chart.clock.no_campaign")
		for l: Label in [_day, _act_label, _days, _state]:
			l.text = ""
		_hunter.visible = false
		_continue.visible = false
		_pause.visible = false
		_speed_button.visible = false
		state_changed.emit()
		return
	_speed_button.visible = true
	var hour := Kit.get_int(_chart, "hour")
	_dtg.text = _dtg_text(hour)
	_day.text = tr(&"chart.clock.day").format([Kit.number(Kit.get_int(_chart, "day", 1), &"none", 0), _hour_text(hour), _word(&"time", Kit.get_int(_chart, "time_of_day"))])
	var act := Kit.get_int(_chart, "act")
	var act_line := tr(&"chart.clock.act").format([_word(&"act", act), tr(StringName("act.%d.name" % (act + 1)))])
	if Kit.get_bool(_chart, "retreat"):
		act_line = tr(&"chart.clock.act_retreat").format([act_line])
	_act_label.text = act_line
	var hours_left := Kit.get_float(_chart, "clock_hours_left", -1.0)
	if Kit.get_bool(_chart, "open_theatre"):
		_days.text = tr(&"chart.clock.open_theatre")
	elif hours_left < 0.0:
		_days.text = tr(&"chart.clock.no_clock")
	else:
		var days := hours_left / 24.0
		_days.text = tr(&"chart.clock.days_left").format([Kit.number(floorf(days * 10.0) / 10.0, &"none", 1 if days < 10.0 else 0)])
	var advancing := _advancing()
	var on_passage := not advancing and not Kit.get_bool(_chart, "at_cove", true) and Kit.get_int(_chart, "phase", -1) != PHASE_SORTIE
	_continue.visible = not advancing and (_pending or on_passage)
	_pause.visible = advancing
	_pause.text = tr(&"chart.clock.resume") if _held else tr(&"chart.clock.pause")
	_pause.tooltip_text = tr(&"chart.clock.resume_tip") if _held else tr(&"chart.clock.pause_tip")
	if advancing:
		_state.text = tr(&"chart.clock.held") if _held else tr(&"chart.clock.running")
	elif not _interrupt_text.is_empty():
		_state.text = tr(&"chart.clock.stopped").format([_msg(_interrupt_text)])
	else:
		_state.text = tr(&"chart.clock.waiting")
	_state.tooltip_text = _state.text
	var warn := Kit.get_int(_chart, "hunter_warning_h", -1)
	_hunter.visible = warn >= 0
	if warn >= 0:
		_hunter.text = tr(&"chart.clock.hunter").format([Kit.number(warn, &"h", 0)])
		_hunter.tooltip_text = tr(&"chart.clock.hunter_tip")
	state_changed.emit()


func _on_continue() -> void:
	if _source == null:
		return
	if _source.call(&"advance") == true:
		_pending = false
		_interrupt_text = {}
	_render()


func _on_pause() -> void:
	if _source == null:
		return
	_held = not _held
	_source.call(&"set_clock_paused", _held)
	_render()
	Ui.focus(_pause)


## The `pause` action on the chart table holds or releases running time.
func toggle_hold() -> bool:
	if not _pause.visible:
		return false
	_on_pause()
	return true


func _on_speed(index: int) -> void:
	if index < 0 or index >= SPEEDS.size():
		return
	_speed = SPEEDS[index]
	if _source != null:
		_source.call(&"set_advance_speed", _speed)


func _on_stack_changed(depth: int) -> void:
	if depth != 1 or not _held:
		return
	# The pause menu releases the clock when it leaves the tree, after this signal; put the
	# player's own hold back on the next frame.
	await get_tree().process_frame
	if is_inside_tree() and _held and _source != null and _advancing():
		_source.call(&"set_clock_paused", true)


static func _word(kind: StringName, value: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"word"):
		return String(tx.call(&"word", kind, value))
	return ""


static func _msg(m: Dictionary) -> String:
	return preload("res://ui/widgets/ui_text.gd").resolve(m)


static func _dtg_text(hour: int) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"date_time_group"):
		return String(tx.call(&"date_time_group", hour))
	return ""


static func _hour_text(hour: int) -> String:
	return "%02d:00" % (posmod(hour, 24))
