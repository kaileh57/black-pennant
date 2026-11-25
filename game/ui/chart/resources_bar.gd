extends HBoxContainer
## Shared pool: Salvage, Intel, fuel, ammunition.
## DESIGN.md L811-822. Unit: ui-chart-panels.
## Reads and calls: Campaign.resources, Campaign.resources_changed.
##
## The one pool on SEXTANT (L837), as exact readouts with their units merged into the value
## ("400 salvage", L1701), which the gauge rule exempts (L1597). Each tooltip says what the resource
## is spent on. A value that changes gets the box highlight for the mode-change time (L1565, L1649),
## so a payout or a purchase shows where it landed; nothing counts up or blinks.

const Kit := preload("res://ui/screens/menu_kit.gd")

var _source: RefCounted
var _values: Dictionary[StringName, float] = {}
## Label name to the msec its box comes down.
var _boxes: Dictionary[StringName, int] = {}
var _timer: Timer
var _has_data := false

@onready var _labels: Dictionary[StringName, Label] = {
	&"salvage": $Salvage, &"intel": $Intel, &"fuel": $Fuel, &"ammo": $Ammo,
}


func _ready() -> void:
	Ui.set_separation(self, 5)
	for key: StringName in _labels:
		var l := _labels[key]
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		l.text = ""
		l.tooltip_text = tr(StringName("chart.res.%s_tip" % key))
	_timer = Timer.new()
	_timer.one_shot = true
	_timer.timeout.connect(_on_timer)
	add_child(_timer)
	accessibility_name = tr(&"chart.res.name")


func bind(source: RefCounted) -> void:
	_source = source


## Re-read the pool (on `Campaign.resources_changed`).
func refresh() -> void:
	if _source == null:
		return
	show_resources(_source.call(&"resources"))


## Show a `ResourcesDict`; values that differ from the last shown ones get the box.
func show_resources(r: Dictionary) -> void:
	if not is_node_ready():
		return
	if r.is_empty():
		for l: Label in _labels.values():
			l.text = ""
		_values.clear()
		_has_data = false
		queue_redraw()
		return
	var now: Dictionary[StringName, float] = {
		&"salvage": float(Kit.get_int(r, "salvage")),
		&"intel": float(Kit.get_int(r, "intel")),
		&"fuel": Kit.get_float(r, "fuel_l"),
		&"ammo": Kit.get_float(r, "ammo_kg"),
	}
	_labels[&"salvage"].text = Kit.number(now[&"salvage"], &"salvage", 0)
	_labels[&"intel"].text = Kit.number(now[&"intel"], &"intel", 0)
	_labels[&"fuel"].text = tr(&"chart.res.fuel").format([Kit.number(roundf(now[&"fuel"]), &"l", 0)])
	_labels[&"ammo"].text = tr(&"chart.res.ammo").format([Kit.number(now[&"ammo"] / 1000.0, &"t", 1)])
	if _has_data:
		var until := Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0)
		for key: StringName in now:
			if absf(now[key] - _values.get(key, now[key])) > 0.5:
				_boxes[key] = until
		_arm()
	_values = now
	_has_data = true
	queue_redraw()


func boxed() -> Array[StringName]:
	var out: Array[StringName] = []
	var t := Time.get_ticks_msec()
	for key: StringName in _boxes:
		if _boxes[key] > t:
			out.append(key)
	return out


func _draw() -> void:
	var t := Time.get_ticks_msec()
	var w := float(Ui.token("stroke.frame", 2))
	for key: StringName in _boxes:
		if _boxes[key] <= t:
			continue
		var l := _labels[key]
		draw_rect(Rect2(l.position, l.size).grow(w + 2.0), Ui.colour(&"ui.box"), false, w)


func _arm() -> void:
	var t := Time.get_ticks_msec()
	var soonest := -1
	for key: StringName in _boxes.keys():
		if _boxes[key] <= t:
			_boxes.erase(key)
		elif soonest < 0 or _boxes[key] < soonest:
			soonest = _boxes[key]
	if soonest > 0 and _timer.is_inside_tree():
		_timer.start(maxf(0.05, float(soonest - t) / 1000.0))


func _on_timer() -> void:
	_arm()
	queue_redraw()
