extends VBoxContainer
## Sortie conditions for skirmish and sandbox setup: weather, sea state, time of day and seed.
## DESIGN.md L959-961 (modes), L1975-2035 (weather and time-of-day presets). Units: ui-skirmish.
## Built in code; add it with `ConditionsPanel.new()` and read `values()`.
##
## Defaults are the look the game is built around: clear weather, sea state 3 and dusk, the low warm
## sun (Graphics and rendering). Sea state is the WMO code 0 to 8 with its WMO name. The seed is
## blank for random; a number is used as it is and any other text is hashed, so a word can be shared.

const Kit := preload("res://ui/screens/menu_kit.gd")

## `Bp.WEATHER_*` in order, and `Bp.TIME_*` in the order offered (dusk first: the hero light).
const WEATHERS: Array[int] = [0, 1, 2, 3, 4, 5]
const WEATHER_WORDS: Array[StringName] = [&"clear", &"haze", &"overcast", &"rain", &"storm", &"fog_bank"]
const TIMES: Array[int] = [1, 2, 0]
const TIME_WORDS: Dictionary[int, StringName] = {0: &"night", 1: &"dusk", 2: &"day"}
const SEA_MAX := 8
const DEFAULT_SEA := 3

signal changed

var weather: OptionButton
var sea: HSlider
var sea_text: Label
var time_of_day: OptionButton
var seed_field: LineEdit


func _init() -> void:
	Ui.set_separation(self, 2)
	add_child(Kit.label("skirmish.conditions", &"PanelTitle"))
	weather = OptionButton.new()
	weather.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for i in WEATHERS.size():
		weather.add_item(Kit.tr_key(StringName("units.word.weather." + String(WEATHER_WORDS[i]))), WEATHERS[i])
	weather.select(0)
	weather.item_selected.connect(func(_i: int) -> void: changed.emit())
	add_child(_row("skirmish.weather", weather))
	sea = preload("res://ui/widgets/focus_slider.gd").new()
	sea.min_value = 0
	sea.max_value = SEA_MAX
	sea.step = 1
	sea.value = DEFAULT_SEA
	sea.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	sea.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	sea_text = Kit.label("", &"Body", true)
	sea_text.custom_minimum_size = Vector2(200.0, 0.0)
	sea_text.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	sea.value_changed.connect(_on_sea)
	var sea_row := _row("skirmish.sea_state", sea)
	sea_row.add_child(sea_text)
	add_child(sea_row)
	time_of_day = OptionButton.new()
	time_of_day.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	for t: int in TIMES:
		time_of_day.add_item(Kit.tr_key(StringName("units.word.time." + String(TIME_WORDS[t]))), t)
	time_of_day.select(0)
	time_of_day.item_selected.connect(func(_i: int) -> void: changed.emit())
	add_child(_row("skirmish.time_of_day", time_of_day))
	seed_field = LineEdit.new()
	seed_field.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	seed_field.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	seed_field.max_length = 32
	seed_field.placeholder_text = Kit.tr_key(&"skirmish.seed.placeholder")
	add_child(_row("skirmish.seed", seed_field))
	_on_sea(sea.value)


func _row(label_key: String, control: Control) -> HBoxContainer:
	var row := Kit.hbox(3)
	var l := Kit.label(label_key, &"Body")
	l.custom_minimum_size = Vector2(220.0, 0.0)
	l.size_flags_vertical = Control.SIZE_SHRINK_CENTER
	row.add_child(l)
	if control is OptionButton:
		control.custom_minimum_size = Vector2(240.0, Kit.BUTTON_H)
	control.accessibility_name = Kit.tr_key(StringName(label_key))
	row.add_child(control)
	return row


func _on_sea(v: float) -> void:
	sea_text.text = sea_word(int(v))
	sea.accessibility_description = sea_text.text
	changed.emit()


## "3, slight" style text for a WMO sea-state code.
static func sea_word(code: int) -> String:
	var c := clampi(code, 0, SEA_MAX)
	return Kit.tr_key(&"skirmish.sea.row").format([c, Kit.tr_key(StringName("skirmish.sea.%d" % c))])


## The focusable controls top to bottom, for the screen's focus chain.
func focus_controls() -> Array[Control]:
	return [weather, sea, time_of_day, seed_field]


## `{ "weather": int, "sea_state": int, "time_of_day": int, "seed": int }` for the session setup.
func values() -> Dictionary:
	return {
		"weather": weather.get_selected_id() if weather.selected >= 0 else 0,
		"sea_state": int(sea.value),
		"time_of_day": time_of_day.get_selected_id() if time_of_day.selected >= 0 else 1,
		"seed": seed_value(seed_field.text),
	}


## Blank is 0 (random); a whole number is used as it is; other text is hashed into a seed.
static func seed_value(text: String) -> int:
	var t := text.strip_edges()
	if t.is_empty():
		return 0
	if t.is_valid_int():
		return absi(t.to_int())
	return absi(hash(t)) % 2147483647 + 1
