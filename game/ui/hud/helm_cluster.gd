extends PanelContainer
## Own-ship gauges: speed, heading, heel, trim, fuel, power.
## DESIGN.md L1668. Unit: ui-hud-ship.
## Reads and calls: Hud.motion, Hud.motion_limits, Hud.power, Hud.motion_changed.
##
## Every gauge is a value with its unit against its band and limits, with a trend where the rate
## matters (rule 12); the value is the brightest thing on it. Six gauges in two columns:
## - Speed: astern to the design's top speed, band ahead; the ordered speed is a caret under the
##   scale and a line under it; trend: acceleration over 10 s.
## - Heading: a compass tape 90 degrees wide around the heading, ticks every 5 degrees, labels every
##   30 (a labelled graticule, rule 13), lubber line at the centre, the ordered heading as a caret
##   (at the edge, pointing out, when it is off the tape), rate of turn as a trend over 10 s. The
##   line under it gives the ordered heading and the rudder.
## - Heel: symmetric, caution and limit from `Hud.motion_limits` (the capsize-imminent heel and half
##   of it), band half the caution heel; the label says which side; trend over 5 s.
## - Trim: +-12 degrees, band 3, caution 6, limit 10 (a design choice; the spec gives no trim
##   limit, and a planing hull runs 3 to 5 degrees bow up at speed, so a normal trim stays quiet);
##   bow up or down in the label; trend over 10 s.
## - Fuel: 0 to 100%, caution at 20%, limit at 10%; the trend is the burn rate; the line gives the
##   litres and the endurance at that rate.
## - Power demand against generation: caution at 90%, limit at 100% of generation (the Engineer
##   sheds load above it, L287); the line gives the supply and the shed consumers. No trend: power
##   steps with orders, so the gauge waives it.
## Speed and heading always show: they are what the player reads at a glance. Heel, trim, fuel and
## power show with the route line when the player opens them (the chevron in the header, kept for
## the session), and any of the four shows by itself while it is past its caution limit, so a
## normal cluster stays small and quiet (rule 1) and a problem still reads without a click.
## Rates come from successive readings over sortie time (`Session.time_s`), so compression and pause
## do not distort them. The header shows who holds the helm (Officer, or Player with the hand glyph,
## rule 9) with the take-the-helm button and its key; the route line gives the waypoints left and
## the next one's bearing and range. States: waiting for the first picture, no hull, populated,
## disabled or lost hull. `show_motion`, `show_power`, `show_limits` and `show_holder` render plain
## data (tests and captures); `refresh()` pulls everything.

const Kit := preload("res://ui/screens/menu_kit.gd")
const GaugeScene := preload("res://ui/widgets/gauge.tscn")
const HintScene := preload("res://ui/widgets/shortcut_hint.tscn")

## Rates are smoothed over this much sortie time.
const RATE_TAU_S := 0.5
const TRIM_SCALE_DEG := 12.0
const TRIM_CAUTION_DEG := 6.0
const TRIM_LIMIT_DEG := 10.0
const FUEL_BAND := 25.0
const FUEL_CAUTION := 20.0
const FUEL_LIMIT := 10.0
const POWER_CAUTION := 0.9
## Below this the heel, trim and rudder read as level, amidships.
const LEVEL_DEG := 0.5
const AMIDSHIPS := 0.02
## Two gauge columns need this much width; narrower (large UI scale) the gauges stack.
const TWO_COLUMN_W := 340.0
## Narrower than this, the header drops the holder word (the button still names the state).
const HOLDER_W := 420.0
## A gauge's narrowest width: label and value side by side over the scale.
const GAUGE_MIN_W := 150.0
## The gauges scroll past this share of the window height (floor 2: no clipping at 200%).
const MAX_HEIGHT_SHARE := 0.40

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column
@onready var _header: HFlowContainer = $Margin/Column/Header
@onready var _hand: TextureRect = $Margin/Column/Header/Hand
@onready var _holder: Label = $Margin/Column/Header/Holder
@onready var _take: Button = $Margin/Column/Header/Take
@onready var _more_button: Button = $Margin/Column/Header/More
@onready var _status: Label = $Margin/Column/Status
@onready var _scroll: ScrollContainer = $Margin/Column/Scroll
@onready var _gauges: VBoxContainer = $Margin/Column/Scroll/Gauges
@onready var _grid: GridContainer = $Margin/Column/Scroll/Gauges/Grid
@onready var _more: GridContainer = $Margin/Column/Scroll/Gauges/More
@onready var _route: Label = $Margin/Column/Route
@onready var _empty: Label = $Margin/Column/Empty

var _speed: Gauge
var _speed_mark: OrderMark
var _speed_line: Label
var _tape: HeadingTape
var _heading_line: Label
var _heel: Gauge
var _trim: Gauge
var _fuel: Gauge
var _fuel_line: Label
var _power: Gauge
var _power_line: Label
var _hint: Control
## Height the HUD shell leaves the cluster (`set_max_height`); -1 falls back to the screen share.
var _max_height := -1.0
## The secondary gauges' cells: heel, trim, fuel, power.
var _more_cells: Array[Control] = []

## Heel, trim, fuel and power open, kept for the session.
static var expanded := false

var _limits: Dictionary = {}
var _motion: Dictionary = {}
var _power_data: Dictionary = {}
var _held := false
var _vessel := Bp.NONE
var _speed_rate := RateMeter.new()
var _heading_rate := RateMeter.new()
var _heel_rate := RateMeter.new()
var _trim_rate := RateMeter.new()


func _ready() -> void:
	# The panel's own 16 px margin is the inset; the gauges keep the rest of the width.
	Ui.set_margins(_margin, 0)
	for side: StringName in [&"margin_left", &"margin_right", &"margin_top", &"margin_bottom"]:
		_margin.add_theme_constant_override(side, 0)
	Ui.set_separation(_column, 1)
	_header.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	_header.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	Ui.set_separation(_gauges, 1)
	for g: GridContainer in [_grid, _more]:
		g.add_theme_constant_override(&"h_separation", Ui.spacing(2))
		g.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	for l: Label in [_holder, _status, _route, _empty]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_take.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_take.focus_mode = Control.FOCUS_ALL
	_take.pressed.connect(_on_take)
	_hand.texture = Ui.icon(&"hand")
	# The key hint names the binding; the headless display server cannot name keys, so a headless
	# run (the compile check) leaves it out.
	if DisplayServer.get_name() != "headless":
		_hint = HintScene.instantiate()
		_hint.set(&"compact", true)
		_hint.set(&"action", &"take_helm")
		_hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		_header.add_child(_hint)
		_header.move_child(_hint, _take.get_index())
	_more_button.focus_mode = Control.FOCUS_ALL
	_more_button.flat = true
	_more_button.set_pressed_no_signal(expanded)
	_more_button.toggled.connect(_on_more)
	_build_cells()
	_show_more()
	resized.connect(_fit)
	_gauges.minimum_size_changed.connect(_fit)
	get_viewport().size_changed.connect(_fit)
	accessibility_name = tr(&"hud_ship.helm.title")
	_on(&"Hud", &"motion_changed", _pull_motion)
	_on(&"Hud", &"power_changed", _pull_power)
	_on(&"Hud", &"ship_static_changed", refresh)
	_on(&"Hud", &"board_changed", _pull_holder)
	_on(&"Hud", &"control_changed", func(_r: Dictionary, _h: int) -> void: _pull_holder())
	_on(&"Session", &"state_changed", func(_s: int) -> void: refresh())
	if Ui.has_signal(&"changed"):
		Ui.changed.connect(_on_ui_changed)
	_apply_backing()
	_apply_theme_colours()
	refresh()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_TRANSLATION_CHANGED:
			if is_node_ready():
				_label_gauges()
				_tape.reload_labels()
				show_holder(_held)
				show_motion(_motion)
				show_power(_power_data)
		NOTIFICATION_THEME_CHANGED:
			if is_node_ready():
				_apply_theme_colours()


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _on_ui_changed(what: StringName) -> void:
	if what == &"backing" or what == &"transparency":
		_apply_backing()
	elif what == &"palette":
		_apply_theme_colours()
	elif what == &"scale":
		_fit.call_deferred()


## The cluster's height with its gauges uncapped by `set_max_height` (still within the screen share).
func natural_height() -> float:
	var chrome := get_combined_minimum_size().y - _scroll.get_combined_minimum_size().y
	var cap := get_viewport().get_visible_rect().size.y * MAX_HEIGHT_SHARE
	return chrome + ceilf(minf(_gauges.get_combined_minimum_size().y, cap))


## Controls that take focus, left to right: take or hand back the helm, then the chevron that shows
## or hides heel, trim, fuel and power (the HUD shell links panels with them).
func focus_targets() -> Array:
	var out: Array = []
	for c: Control in [_take, _more_button]:
		if c.is_visible_in_tree():
			out.append(c)
	if out.size() == 2:
		Kit.chain_horizontal(out)
	return out


## The height the HUD shell leaves the cluster; past it the gauges scroll (floor 2).
func set_max_height(height: float) -> void:
	if is_equal_approx(height, _max_height):
		return
	_max_height = height
	_fit()


## The panel width two gauge columns need: two gauges, the gap and the panel's margins.
func _two_column_need() -> float:
	var panel := get_theme_stylebox(&"panel")
	var frame := panel.get_margin(SIDE_LEFT) + panel.get_margin(SIDE_RIGHT) if panel != null else 0.0
	var cell := GAUGE_MIN_W
	for g: GridContainer in [_grid, _more]:
		for c: Node in g.get_children():
			cell = maxf(cell, (c as Control).get_combined_minimum_size().x)
	return cell * 2.0 + float(_grid.get_theme_constant(&"h_separation")) + frame


## Two columns of gauges while the panel is wide enough, else one; the gauges scroll rather than
## run off the window at large UI scales.
func _fit() -> void:
	if not is_inside_tree():
		return
	var cols := 2 if size.x >= TWO_COLUMN_W and size.x >= _two_column_need() else 1
	# In a narrow centre (a large UI scale) the holder word gives way so the header keeps one row;
	# the button's own words (Take helm, Hand back helm) and the hand glyph still say who has it.
	_holder.visible = _grid.visible and size.x >= HOLDER_W
	for g: GridContainer in [_grid, _more]:
		if g.columns != cols:
			g.columns = cols
	var cap := get_viewport().get_visible_rect().size.y * MAX_HEIGHT_SHARE
	if _max_height >= 0.0:
		var chrome := get_combined_minimum_size().y - _scroll.get_combined_minimum_size().y
		cap = minf(cap, maxf(_max_height - chrome, 0.0))
	var want := Vector2(0.0, ceilf(minf(_gauges.get_combined_minimum_size().y, cap)))
	if not want.is_equal_approx(_scroll.custom_minimum_size):
		_scroll.custom_minimum_size = want


func _apply_backing() -> void:
	self_modulate.a = 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)


func _apply_theme_colours() -> void:
	_hand.self_modulate = Ui.colour(&"text.primary")


func _cell(top: Control, more: bool = false) -> VBoxContainer:
	var box := VBoxContainer.new()
	box.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	box.add_theme_constant_override(&"separation", 0)
	box.add_child(top)
	if more:
		_more.add_child(box)
		_more_cells.append(box)
		if top is Gauge:
			(top as Gauge).state_changed.connect(func(_s: int) -> void: _show_more())
	else:
		_grid.add_child(box)
	return box


func _on_more(on: bool) -> void:
	expanded = on
	_show_more()


## The secondary gauges: all of them while open, else only those past their caution limit.
func _show_more() -> void:
	_more_button.icon = Ui.icon(&"chevron_down" if expanded else &"chevron_up")
	var tip := tr(&"hud_ship.helm.fewer_tooltip" if expanded else &"hud_ship.helm.more_tooltip")
	_more_button.tooltip_text = tip
	_more_button.accessibility_name = tip
	var any := false
	for box: Control in _more_cells:
		var g := box.get_child(0) as Gauge
		var alarm := g != null and (g.state() == Gauge.State.CAUTION or g.state() == Gauge.State.WARNING)
		box.visible = expanded or alarm
		any = any or box.visible
	_more.visible = any
	_route.visible = expanded and not _route.text.is_empty() and _grid.visible


func _line(box: VBoxContainer) -> Label:
	var l := Kit.label("", &"Dense", true)
	l.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	l.mouse_filter = Control.MOUSE_FILTER_IGNORE
	box.add_child(l)
	return l


func _gauge() -> Gauge:
	var g: Gauge = GaugeScene.instantiate()
	g.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	g.custom_minimum_size = Vector2(GAUGE_MIN_W, 0)
	return g


func _build_cells() -> void:
	_speed = _gauge()
	var box := _cell(_speed)
	_speed_mark = OrderMark.new()
	box.add_child(_speed_mark)
	_speed_line = _line(box)
	_tape = HeadingTape.new()
	_tape.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	box = _cell(_tape)
	_heading_line = _line(box)
	_heel = _gauge()
	_cell(_heel, true)
	_trim = _gauge()
	_cell(_trim, true)
	_fuel = _gauge()
	box = _cell(_fuel, true)
	_fuel_line = _line(box)
	_power = _gauge()
	box = _cell(_power, true)
	_power_line = _line(box)
	_speed.configure({"unit": &"kn", "decimals": 0, "min": -24.0, "max": 40.0, "band_low": 0.0, "band_high": 40.0, "trend_s": 10.0})
	_heel.configure({"unit": &"deg", "decimals": 0, "trend_s": 5.0})
	_trim.configure({"unit": &"deg", "decimals": 1, "min": -TRIM_SCALE_DEG, "max": TRIM_SCALE_DEG, "band_low": -TRIM_CAUTION_DEG * 0.5, "band_high": TRIM_CAUTION_DEG * 0.5,
		"caution_low": -TRIM_CAUTION_DEG, "caution_high": TRIM_CAUTION_DEG, "warning_low": -TRIM_LIMIT_DEG, "warning_high": TRIM_LIMIT_DEG, "trend_s": 10.0})
	_fuel.configure({"unit": &"pct", "decimals": 0, "min": 0.0, "max": 100.0, "band_low": FUEL_BAND, "caution_low": FUEL_CAUTION, "warning_low": FUEL_LIMIT, "trend_s": 600.0})
	_power.configure({"unit": &"kw", "decimals": 0, "min": 0.0, "max": 1000.0, "trend_s": 0.0, "trend_waiver": &"hud_ship.gauge.power_waiver"})
	_heel_limits(25.0, 50.0)
	_label_gauges()


func _label_gauges() -> void:
	_speed.label = &"hud_ship.gauge.speed"
	_tape.label_text = tr(&"hud_ship.gauge.heading")
	_heel.label = _heel_label(_heel.value)
	_trim.label = _trim_label(_trim.value)
	_fuel.label = &"hud_ship.gauge.fuel"
	_power.label = &"hud_ship.gauge.power"


func _heel_limits(caution: float, limit: float) -> void:
	var c := caution if is_finite(caution) and caution > 0.0 else 25.0
	var l := limit if is_finite(limit) and limit > c else c * 2.0
	_heel.configure({"min": -l * 1.2, "max": l * 1.2, "band_low": -c * 0.5, "band_high": c * 0.5,
		"caution_low": -c, "caution_high": c, "warning_low": -l, "warning_high": l})


# --- data ----------------------------------------------------------------------------------------

func refresh() -> void:
	if not is_node_ready():
		return
	show_limits(Kit.as_dict(Kit.call_on(&"Hud", &"motion_limits")))
	_pull_holder()
	_pull_power()
	_pull_motion()


func _pull_motion() -> void:
	show_motion(Kit.as_dict(Kit.call_on(&"Hud", &"motion")), _sortie_time())


func _pull_power() -> void:
	show_power(Kit.as_dict(Kit.call_on(&"Hud", &"power")))


func _pull_holder() -> void:
	var v: Variant = Kit.call_on(&"Hud", &"role_holder", [Bp.ROLE_HELM, 0])
	show_holder(v is int and int(v) == Bp.HOLDER_PLAYER)


func _sortie_time() -> float:
	var v: Variant = Kit.call_on(&"Session", &"time_s")
	return float(v) if (v is float or v is int) and is_finite(float(v)) else NAN


## Gauge limits for the design (`Hud.motion_limits`).
func show_limits(limits: Dictionary) -> void:
	_limits = limits
	if not is_node_ready():
		return
	var top := Kit.get_float(limits, "max_speed_kn", 0.0)
	if not (top > 0.0 and is_finite(top)):
		top = 40.0
	var astern := Kit.get_float(limits, "astern_speed_kn", top * 0.6)
	if not (astern >= 0.0 and is_finite(astern)):
		astern = top * 0.6
	# The scale runs a little past top speed so a following sea's surplus still reads.
	_speed.configure({"min": -astern, "max": top * 1.1, "band_low": 0.0, "band_high": top})
	_heel_limits(Kit.get_float(limits, "heel_caution_deg", 25.0), Kit.get_float(limits, "heel_limit_deg", 50.0))


## Render a `MotionDict` read at sortie time `time_s` (NAN: no rate).
func show_motion(motion: Dictionary, time_s: float = NAN) -> void:
	_motion = motion
	if not is_node_ready():
		return
	var has := not motion.is_empty()
	var vessel := Kit.get_int(motion, "vessel", Bp.NONE)
	if vessel != _vessel:
		_vessel = vessel
		for m: RateMeter in [_speed_rate, _heading_rate, _heel_rate, _trim_rate]:
			m.reset()
	_grid.visible = has
	_scroll.visible = has
	_take.visible = has
	_more_button.visible = has
	if _hint != null:
		_hint.visible = has
	_holder.visible = has
	_hand.visible = has and _held
	_empty.visible = not has
	if not has:
		_empty.text = tr(_empty_key())
		_status.visible = false
		_route.visible = false
		return
	var speed := Kit.get_float(motion, "speed_kn", 0.0)
	var heading := wrapf(Kit.get_float(motion, "heading_deg", 0.0), 0.0, 360.0)
	var heel := Kit.get_float(motion, "heel_deg", 0.0)
	var trim := Kit.get_float(motion, "trim_deg", 0.0)
	_speed.decimals = 1 if absf(speed) < 9.95 else 0
	_speed.set_reading(speed, _speed_rate.feed(speed, time_s, false))
	var ordered_speed := Kit.get_float(motion, "ordered_speed_kn", NAN)
	_speed_mark.show_value(ordered_speed, _speed.min_value, _speed.max_value)
	_speed_line.text = tr(&"hud_ship.speed.ordered").format([Kit.number(ordered_speed, &"kn", 1 if absf(ordered_speed) < 9.95 else 0)]) if is_finite(ordered_speed) else ""
	var ordered_heading := Kit.get_float(motion, "ordered_heading_deg", NAN)
	_tape.show_heading(heading, ordered_heading, _heading_rate.feed(heading, time_s, true), _bearing(heading))
	_heading_line.text = heading_line(ordered_heading, Kit.get_float(motion, "rudder", 0.0))
	var old_heel := _heel.value
	_heel.set_reading(heel, _heel_rate.feed(heel, time_s, false))
	if _side(old_heel) != _side(heel):
		_heel.label = _heel_label(heel)
	var old_trim := _trim.value
	_trim.set_reading(trim, _trim_rate.feed(trim, time_s, false))
	if _side(old_trim) != _side(trim):
		_trim.label = _trim_label(trim)
	var status := Kit.get_int(motion, "status", Bp.VESSEL_ACTIVE)
	_status.visible = status != Bp.VESSEL_ACTIVE
	_status.text = tr(status_key(status))
	var route := route_line(motion)
	_route.text = route
	_route.visible = expanded and not route.is_empty()


func _empty_key() -> StringName:
	var state := int(Kit.call_on(&"Session", &"state")) if Kit.call_on(&"Session", &"state") is int else Bp.SESSION_IDLE
	if state == Bp.SESSION_IN_SORTIE or state == Bp.SESSION_LOADING:
		return &"hud_ship.helm.waiting"
	return &"hud_ship.helm.no_hull"


## Render a `PowerDict`.
func show_power(power: Dictionary) -> void:
	_power_data = power
	if not is_node_ready() or power.is_empty():
		return
	var fraction := clampf(Kit.get_float(power, "fuel", 0.0), 0.0, 1.0)
	var litres := maxf(Kit.get_float(power, "fuel_l", 0.0), 0.0)
	var flow := maxf(Kit.get_float(power, "fuel_flow_lps", 0.0), 0.0)
	var capacity := litres / fraction if fraction > 0.0001 else 0.0
	var fuel_rate := -flow / capacity * 100.0 if capacity > 0.0 else NAN
	_fuel.set_reading(fraction * 100.0, fuel_rate)
	_fuel_line.text = fuel_line(litres, flow)
	_fuel.tooltip_text = tr(&"hud_ship.fuel.litres").format([Kit.number(litres, &"l", 0)])
	var gen := maxf(Kit.get_float(power, "generation_kw", 0.0), 0.0)
	var demand := maxf(Kit.get_float(power, "demand_kw", 0.0), 0.0)
	var supplied := maxf(Kit.get_float(power, "supplied_kw", 0.0), 0.0)
	var top := maxf(gen * 1.25, demand * 1.05)
	if top <= 0.0:
		top = 1.0
	if gen > 0.0:
		_power.configure({"max": top, "band_low": 0.0, "band_high": gen * POWER_CAUTION, "caution_high": gen * POWER_CAUTION, "warning_high": gen})
	else:
		_power.configure({"max": top, "band_low": NAN, "band_high": NAN, "caution_high": NAN, "warning_high": NAN})
	_power.set_reading(demand)
	_power_line.text = power_line(supplied, gen, Kit.get_int(power, "shed_count", 0), demand)


## Who holds the helm (rule 9).
func show_holder(player: bool) -> void:
	_held = player
	if not is_node_ready():
		return
	_holder.text = tr(&"hud_ship.holder.player" if player else &"hud_ship.holder.officer")
	_hand.visible = player and _grid.visible
	_take.text = tr(&"hud_ship.helm.release" if player else &"hud_ship.helm.take")
	_take.tooltip_text = tr(&"hud_ship.helm.release_tooltip" if player else &"hud_ship.helm.take_tooltip")


func _on_take() -> void:
	Kit.call_on(&"Orders", &"take_control", [Bp.ROLE_HELM, 0, not _held])


# --- text (static so tests share them) -----------------------------------------------------------

static func _bearing(deg: float) -> String:
	return HelmText.bearing(deg)


static func _range(m: float) -> String:
	var v: Variant = Kit.call_on(&"Tx", &"range", [m])
	if v is String and not (v as String).is_empty():
		return v
	return Kit.number(m / 1000.0, &"km", 1)


static func _side(v: float) -> int:
	if not is_finite(v) or absf(v) < LEVEL_DEG:
		return 0
	return 1 if v > 0.0 else -1


static func _heel_label(v: float) -> StringName:
	match _side(v):
		1: return &"hud_ship.gauge.heel_starboard"
		-1: return &"hud_ship.gauge.heel_port"
	return &"hud_ship.gauge.heel"


static func _trim_label(v: float) -> StringName:
	match _side(v):
		1: return &"hud_ship.gauge.trim_bow_up"
		-1: return &"hud_ship.gauge.trim_bow_down"
	return &"hud_ship.gauge.trim"


## "Ordered 060, rudder 20% to port".
static func heading_line(ordered_deg: float, rudder: float) -> String:
	var r := clampf(rudder if is_finite(rudder) else 0.0, -1.0, 1.0)
	var rudder_text := TranslationServer.translate(&"hud_ship.rudder.amidships")
	if absf(r) >= AMIDSHIPS:
		rudder_text = TranslationServer.translate(&"hud_ship.rudder.starboard" if r > 0.0 else &"hud_ship.rudder.port").format([Kit.number(absf(r) * 100.0, &"pct", 0)])
	if not is_finite(ordered_deg):
		return rudder_text
	return TranslationServer.translate(&"hud_ship.heading.ordered").format([_bearing(ordered_deg), rudder_text])


## "2 h 10 min left at this rate"; "4,100 l, none burning".
static func fuel_line(litres: float, flow_lps: float) -> String:
	var amount := Kit.number(litres, &"l", 0)
	if flow_lps <= 0.0001 or not is_finite(flow_lps):
		return TranslationServer.translate(&"hud_ship.fuel.idle").format([amount])
	var seconds := litres / flow_lps
	var v: Variant = Kit.call_on(&"Tx", &"duration", [seconds])
	var duration: String = v if v is String and not (v as String).is_empty() else Kit.number(seconds / 60.0, &"min", 0)
	return TranslationServer.translate(&"hud_ship.fuel.endurance").format([duration])


## "Generating 1,000 kW"; "Supplied 820 of 900 kW" when short; ", 2 shed".
static func power_line(supplied: float, generation: float, shed: int, demand: float = 0.0) -> String:
	var text := TranslationServer.translate(&"hud_ship.power.generating").format([Kit.number(generation, &"kw", 0)])
	if supplied + 0.5 < demand:
		text = TranslationServer.translate(&"hud_ship.power.supplied").format([Kit.number(supplied, &"none", 0), Kit.number(demand, &"kw", 0)])
	if shed == 1:
		text = TranslationServer.translate(&"hud_ship.power.shed_one").format([text])
	elif shed > 1:
		text = TranslationServer.translate(&"hud_ship.power.shed_many").format([text, str(shed)])
	return text


## "Route: 3 waypoints, next 060 at 2.4 km"; "" without a route.
static func route_line(motion: Dictionary) -> String:
	var route: Variant = motion.get("route", PackedVector2Array())
	if not route is PackedVector2Array or (route as PackedVector2Array).is_empty():
		return ""
	var points := route as PackedVector2Array
	var pos: Variant = motion.get("pos", Vector2.ZERO)
	var from: Vector2 = pos if pos is Vector2 else Vector2.ZERO
	var d := points[0] - from
	var bearing := wrapf(rad_to_deg(atan2(d.x, -d.y)), 0.0, 360.0)
	var key := &"hud_ship.route.one" if points.size() == 1 else &"hud_ship.route.many"
	return TranslationServer.translate(key).format([str(points.size()), _bearing(bearing), _range(d.length())])


static func status_key(status: int) -> StringName:
	match status:
		Bp.VESSEL_DISABLED: return &"hud_ship.status.disabled"
		Bp.VESSEL_LOST: return &"hud_ship.status.lost"
		Bp.VESSEL_WRECK: return &"hud_ship.status.wreck"
	return &""


# --- drawing -------------------------------------------------------------------------------------

## Smoothed rate of a reading per second of sortie time. Headings wrap at 360.
class RateMeter extends RefCounted:
	var _last := NAN
	var _last_t := NAN
	var rate := NAN

	func reset() -> void:
		_last = NAN
		_last_t = NAN
		rate = NAN

	func feed(v: float, t: float, wraps: bool) -> float:
		if not is_finite(v) or not is_finite(t):
			return rate
		if is_finite(_last) and is_finite(_last_t):
			var dt := t - _last_t
			if dt < 0.0 or dt > 5.0:
				rate = NAN
			elif dt > 0.0:
				var dv := v - _last
				if wraps:
					dv = wrapf(dv, -180.0, 180.0)
				var now := dv / dt
				rate = now if not is_finite(rate) else lerpf(rate, now, clampf(dt / RATE_TAU_S, 0.0, 1.0))
			else:
				return rate
		_last = v
		_last_t = t
		return rate


## The ordered value as a caret under a gauge's scale, aligned with the gauge's inner scale.
class OrderMark extends Control:
	const HEIGHT := 9.0
	var _fraction := NAN

	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_IGNORE
		size_flags_horizontal = Control.SIZE_EXPAND_FILL

	func _get_minimum_size() -> Vector2:
		return Vector2(0.0, HEIGHT)

	func show_value(v: float, lo: float, hi: float) -> void:
		var f := Gauge.scale_fraction(v, lo, hi) if is_finite(v) else NAN
		if (is_nan(f) and is_nan(_fraction)) or is_equal_approx(f, _fraction):
			return
		_fraction = f
		queue_redraw()

	func _draw() -> void:
		if not is_finite(_fraction):
			return
		var rule := float(get_theme_constant(&"stroke_rule", &"Gauge"))
		var x := rule + _fraction * (size.x - rule * 2.0)
		var c := get_theme_color(&"units", &"Gauge")
		draw_colored_polygon(PackedVector2Array([Vector2(x, 0.0), Vector2(x - 5.0, HEIGHT - 1.0), Vector2(x + 5.0, HEIGHT - 1.0)]), c)


## Compass tape for the heading, drawn from the Gauge theme type like the other gauges.
class HeadingTape extends Control:
	const THEME_TYPE := &"Gauge"
	const SPAN_DEG := 90.0
	const SCALE_HEIGHT := 16.0
	const OVERHANG := 4.0
	const ARROW := 6.0
	const TREND_S := 10.0

	var label_text := "":
		set(v):
			label_text = v
			queue_redraw()
	var _heading := NAN
	var _ordered := NAN
	var _rate := NAN
	var _value_text := ""
	var _labels: PackedStringArray = []

	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_PASS
		custom_minimum_size = Vector2(GAUGE_MIN_W, 0)

	func _ready() -> void:
		reload_labels()

	func _notification(what: int) -> void:
		if what == NOTIFICATION_THEME_CHANGED:
			update_minimum_size()
			queue_redraw()

	func reload_labels() -> void:
		_labels.clear()
		for i in 12:
			_labels.append(HelmText.bearing(float(i * 30)))
		queue_redraw()

	func show_heading(heading: float, ordered: float, rate: float, text: String) -> void:
		_heading = heading
		_ordered = ordered
		_rate = rate
		_value_text = text
		accessibility_name = TranslationServer.translate(&"hud_ship.heading.access").format([label_text, text])
		queue_redraw()

	func _get_minimum_size() -> Vector2:
		var vf := get_theme_font(&"value", THEME_TYPE)
		var vs := get_theme_font_size(&"value", THEME_TYPE)
		var sf := get_theme_font(&"scale", THEME_TYPE)
		var ss := get_theme_font_size(&"scale", THEME_TYPE)
		var h := (vf.get_height(vs) if vf != null else 24.0) + float(get_theme_constant(&"spacing_4", THEME_TYPE)) + SCALE_HEIGHT + OVERHANG
		h += (sf.get_height(ss) if sf != null else 22.0) + 2.0
		return Vector2(GAUGE_MIN_W, ceilf(h))

	func _x(deg: float, inner: Rect2) -> float:
		var d := wrapf(deg - _heading, -180.0, 180.0)
		return inner.get_center().x + d / SPAN_DEG * inner.size.x

	func _draw() -> void:
		var w := size.x
		var vf := get_theme_font(&"value", THEME_TYPE)
		var vs := get_theme_font_size(&"value", THEME_TYPE)
		var uf := get_theme_font(&"units", THEME_TYPE)
		var us := get_theme_font_size(&"units", THEME_TYPE)
		var sf := get_theme_font(&"scale", THEME_TYPE)
		var ss := get_theme_font_size(&"scale", THEME_TYPE)
		var line_h := vf.get_height(vs) if vf != null else 24.0
		var ascent := vf.get_ascent(vs) if vf != null else 18.0
		if uf != null and not label_text.is_empty():
			draw_string(uf, Vector2(0.0, ascent), label_text, HORIZONTAL_ALIGNMENT_LEFT, w * 0.6, us, get_theme_color(&"units", THEME_TYPE))
		if vf != null and not _value_text.is_empty():
			draw_string(vf, Vector2(0.0, ascent), _value_text, HORIZONTAL_ALIGNMENT_RIGHT, w, vs, get_theme_color(&"value", THEME_TYPE))
		var top := line_h + float(get_theme_constant(&"spacing_4", THEME_TYPE)) + OVERHANG * 0.5
		var well := Rect2(0.0, top, w, SCALE_HEIGHT)
		draw_rect(well, get_theme_color(&"background", THEME_TYPE), true)
		var rule := float(get_theme_constant(&"stroke_rule", THEME_TYPE))
		draw_rect(well.grow(-rule * 0.5), get_theme_color(&"frame", THEME_TYPE), false, rule)
		if not is_finite(_heading):
			return
		var inner := well.grow(-rule)
		var frame := float(get_theme_constant(&"stroke_frame", THEME_TYPE))
		var major := float(get_theme_constant(&"tick_length", THEME_TYPE))
		var minor := float(get_theme_constant(&"tick_length_minor", THEME_TYPE))
		var ticks := PackedVector2Array()
		var first := ceilf((_heading - SPAN_DEG * 0.5) / 5.0) * 5.0
		var deg := first
		var scale_colour := get_theme_color(&"scale", THEME_TYPE)
		var label_y := well.end.y + OVERHANG * 0.5 + 2.0 + (sf.get_ascent(ss) if sf != null else 16.0)
		while deg <= _heading + SPAN_DEG * 0.5 + 0.001:
			var x := _x(deg, inner)
			var n := int(roundf(wrapf(deg, 0.0, 360.0))) % 360
			var len_px := major if n % 10 == 0 else minor
			ticks.append(Vector2(x, inner.end.y))
			ticks.append(Vector2(x, inner.end.y - len_px))
			if n % 30 == 0 and sf != null and _labels.size() == 12:
				var t := _labels[n / 30]
				var tw := sf.get_string_size(t, HORIZONTAL_ALIGNMENT_LEFT, -1, ss).x
				var lx := clampf(x - tw * 0.5, 0.0, maxf(0.0, w - tw))
				draw_string(sf, Vector2(lx, label_y), t, HORIZONTAL_ALIGNMENT_LEFT, -1, ss, scale_colour)
			deg += 5.0
		if ticks.size() >= 2:
			draw_multiline(ticks, scale_colour, rule)
		# Ordered heading: a caret under the tape, at the edge pointing out when off the tape.
		if is_finite(_ordered):
			var units := get_theme_color(&"units", THEME_TYPE)
			var d := wrapf(_ordered - _heading, -180.0, 180.0)
			var y := well.end.y
			if absf(d) <= SPAN_DEG * 0.5:
				var ox := _x(_ordered, inner)
				draw_colored_polygon(PackedVector2Array([Vector2(ox, y - 5.0), Vector2(ox - 5.0, y + 3.0), Vector2(ox + 5.0, y + 3.0)]), units)
			else:
				var ex := inner.end.x - 1.0 if d > 0.0 else inner.position.x + 1.0
				var s := 1.0 if d > 0.0 else -1.0
				draw_colored_polygon(PackedVector2Array([Vector2(ex, y - 1.0), Vector2(ex - s * 8.0, y - 6.0), Vector2(ex - s * 8.0, y + 4.0)]), units)
		# Lubber line, then the rate of turn.
		var cx := inner.get_center().x
		var needle := get_theme_color(&"needle", THEME_TYPE)
		draw_line(Vector2(cx, well.position.y - OVERHANG * 0.5), Vector2(cx, well.end.y + OVERHANG * 0.5), needle, frame + 1.0)
		if is_finite(_rate):
			var tx := clampf(cx + _rate * TREND_S / SPAN_DEG * inner.size.x, inner.position.x, inner.end.x)
			if absf(tx - cx) >= ARROW:
				var yy := inner.get_center().y
				var trend := get_theme_color(&"trend", THEME_TYPE)
				var dir := signf(tx - cx)
				draw_line(Vector2(cx + dir * 2.0, yy), Vector2(tx, yy), trend, frame)
				draw_colored_polygon(PackedVector2Array([Vector2(tx + dir, yy), Vector2(tx - dir * ARROW, yy - ARROW * 0.6), Vector2(tx - dir * ARROW, yy + ARROW * 0.6)]), trend)


## Bearing text for the tape's inner class, which cannot reach the outer script's statics.
class HelmText extends RefCounted:
	static func bearing(deg: float) -> String:
		var tree := Engine.get_main_loop() as SceneTree
		var tx: Node = tree.root.get_node_or_null(^"Tx") if tree != null and tree.root != null else null
		if tx != null and tx.has_method(&"bearing"):
			var v: Variant = tx.call(&"bearing", deg)
			if v is String and not (v as String).is_empty():
				return v
		return "%03d" % (int(roundf(wrapf(deg, 0.0, 360.0))) % 360)
