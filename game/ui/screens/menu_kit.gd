extends RefCounted
## Shared helpers of the menu, settings, campaign and setup screens: themed controls built in code,
## explicit focus chains (L1738), safe reads of facade results, and number formatting.
## Units: ui-menu, ui-settings, ui-campaign-menu, ui-skirmish. Not a screen; loaded with preload().
##
## Rust facades that are not finished yet fail their calls and give `null`; every reader here takes
## a `Variant` and falls back, so a screen never breaks on a missing or wrong-typed value.

const _Text := preload("res://ui/widgets/ui_text.gd")

## Base screens that keep the menu theme playing. It is the only music and never plays in a sortie,
## the dry dock or the chart table, so leaving these screens for anything else stops it.
const MENU_SCREENS: Array[StringName] = [
	&"main_menu", &"campaign_slots", &"campaign_new", &"skirmish_setup", &"sandbox_setup",
	&"multiplayer", &"replay_browser", &"credits", &"settings",
]

## Height of a menu button: comfortably above the 24 px target floor (L1717) for pointer and pad.
const BUTTON_H := 48.0
## The one prominent action on a screen is taller.
const PRIMARY_H := 64.0


# --- reading facade results ----------------------------------------------------------------------

static func autoload(autoload_name: StringName) -> Node:
	var tree := Engine.get_main_loop() as SceneTree
	if tree == null or tree.root == null:
		return null
	return tree.root.get_node_or_null(NodePath(String(autoload_name)))


## Call `method` on an autoload when it exists and has it; `null` otherwise.
static func call_on(autoload_name: StringName, method: StringName, args: Array = []) -> Variant:
	var node := autoload(autoload_name)
	if node == null or not node.has_method(method):
		return null
	return node.callv(method, args)


static func as_dict(v: Variant) -> Dictionary:
	return v if v is Dictionary else {}


static func as_array(v: Variant) -> Array:
	if v is Array:
		return v
	return []


## Only the Dictionary entries of an array (a malformed entry is dropped, never shown).
static func dicts(v: Variant) -> Array[Dictionary]:
	var out: Array[Dictionary] = []
	for item: Variant in as_array(v):
		if item is Dictionary:
			out.append(item)
	return out


static func get_str(d: Dictionary, key: String, fallback: String = "") -> String:
	var v: Variant = d.get(key, fallback)
	if v is String or v is StringName:
		return String(v)
	return fallback


static func get_int(d: Dictionary, key: String, fallback: int = 0) -> int:
	var v: Variant = d.get(key, fallback)
	if v is int:
		return v
	if v is float and is_finite(v):
		return int(v)
	if v is bool:
		return 1 if v else 0
	return fallback


static func get_float(d: Dictionary, key: String, fallback: float = 0.0) -> float:
	var v: Variant = d.get(key, fallback)
	if (v is float or v is int) and is_finite(float(v)):
		return float(v)
	return fallback


static func get_bool(d: Dictionary, key: String, fallback: bool = false) -> bool:
	var v: Variant = d.get(key, fallback)
	if v is bool:
		return v
	if v is int:
		return v != 0
	return fallback


## The problems a start or load call returned, as displayable text. A `null` result (a facade that
## failed outright) counts as one problem so the player is never left with no answer.
static func problem_texts(result: Variant, fallback_key: StringName) -> PackedStringArray:
	var out := PackedStringArray()
	if not result is Array:
		out.append(tr_key(fallback_key))
		return out
	for item: Variant in result as Array:
		var text := ""
		if item is Dictionary:
			var d := item as Dictionary
			# An IssueDict carries its sentence under `text`; a MsgDict is the sentence.
			text = _Text.resolve(d.get("text")) if d.has("text") else _Text.resolve(d)
		else:
			text = _Text.resolve(item)
		if not text.is_empty():
			out.append(text)
	return out


static func tr_key(key: StringName) -> String:
	return String(TranslationServer.translate(key))


## Start the menu theme (a menu screen opened). Sent every time: a screen this file does not own
## may have stopped it in between.
static func start_menu_theme() -> void:
	call_on(&"AudioFeed", &"set_menu_theme", [true])


## A menu screen that started the theme is leaving: stop it unless the new base is another menu
## screen. `started` is false for a screen that never ran `setup` (the compile check), which then
## calls nothing.
static func leave_menu_theme(started: bool) -> void:
	var screens := autoload(&"Screens")
	if not started or screens == null:
		return
	var base := StringName(screens.call(&"base_name"))
	if not MENU_SCREENS.has(base):
		call_on(&"AudioFeed", &"set_menu_theme", [false])


## Open a screen by name as the new base (setup screens), so a failed start comes back to it.
static func go(screen: StringName, args: Dictionary = {}) -> void:
	var screens := autoload(&"Screens")
	if screens != null:
		screens.call(&"change", screen, args)


static var _busy := false


## Load a campaign save behind the loading cover. On success the campaign phase moves the screens;
## on failure the cover lifts and a message names every problem. Returns true on success.
static func load_campaign(path: String) -> bool:
	if _busy or path.is_empty():
		return false
	_busy = true
	var screens := autoload(&"Screens")
	screens.call(&"show_loading", &"menu.loading.campaign", -1.0)
	await (Engine.get_main_loop() as SceneTree).process_frame
	var problems := problem_texts(call_on(&"Campaign", &"load_slot", [path]), &"menu.load.failed_body")
	_busy = false
	if problems.is_empty():
		screens.call(&"hide_loading")
		return true
	screens.call(&"hide_loading")
	screens.call(&"open_message", &"menu.load.failed_title", problems_body(problems))
	return false


## Resume the suspended sortie (loading it deletes the suspend slot, L826). The session's loading
## state moves the screens; a refusal lifts the cover and says so. Returns true on success.
static func resume_suspend() -> bool:
	if _busy:
		return false
	_busy = true
	var screens := autoload(&"Screens")
	screens.call(&"show_loading", &"menu.loading.resume", -1.0)
	await (Engine.get_main_loop() as SceneTree).process_frame
	var ok: Variant = call_on(&"Campaign", &"resume_suspend")
	_busy = false
	if ok == true:
		return true
	screens.call(&"hide_loading")
	screens.call(&"open_message", &"menu.resume.failed_title", &"menu.resume.failed_body")
	return false


## Text of every problem as one block for a message dialog.
static func problems_body(problems: PackedStringArray) -> String:
	return "\n".join(problems)


## Difficulty presets as players read them, easiest to hardest like the spec's table (L851), so
## a row or a list reads Easy, Normal, Hard while the preset chosen first stays Content's first
## (Normal). Presets other than the three known ones keep their order after them.
static func difficulty_order(presets: Array[Dictionary]) -> Array[Dictionary]:
	const KNOWN: Array[String] = ["easy", "normal", "hard"]
	var out: Array[Dictionary] = []
	for id: String in KNOWN:
		for p: Dictionary in presets:
			if get_str(p, "preset") == id:
				out.append(p)
	for p: Dictionary in presets:
		if not KNOWN.has(get_str(p, "preset")):
			out.append(p)
	return out


# --- numbers -------------------------------------------------------------------------------------

## A number with its unit through `Tx.number`, or a plain locale-formatted number when Tx is absent.
static func number(value: float, unit: StringName, decimals: int) -> String:
	var tx := autoload(&"Tx")
	if tx != null and tx.has_method(&"number"):
		var s: Variant = tx.call(&"number", value, unit, decimals)
		if s is String and not (s as String).is_empty():
			return s
	return TranslationServer.format_number(String.num(value, decimals), TranslationServer.get_locale())


static func percent(fraction: float) -> String:
	return number(roundf(fraction * 100.0), &"pct", 0)


## Decimal places that show a slider step exactly: 0.05 needs 2, 0.1 needs 1, 10 needs 0.
static func decimals_for(step: float) -> int:
	if step <= 0.0 or not is_finite(step):
		return 2
	var d := 0
	var s := step
	while d < 4 and absf(s - roundf(s)) > 0.0001:
		s *= 10.0
		d += 1
	return d


## A local wall-clock time as `YYYY-MM-DD HH:MM` (ISO order reads the same in every locale).
static func local_time(unix: int) -> String:
	if unix <= 0:
		return ""
	var bias := int(Time.get_time_zone_from_system().get("bias", 0))
	var t := Time.get_datetime_dict_from_unix_time(unix + bias * 60)
	return "%04d-%02d-%02d %02d:%02d" % [int(t.year), int(t.month), int(t.day), int(t.hour), int(t.minute)]


# --- controls ------------------------------------------------------------------------------------

## A label in a type variation. `text` is a key (translated by the engine and re-translated on a
## locale change) unless `literal` is true, for text that is already formatted.
static func label(text: String, variation: StringName = &"Body", literal: bool = false) -> Label:
	var l := Label.new()
	l.theme_type_variation = variation
	l.text = text
	l.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	if literal:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	return l


## A menu button: an effect verb from a text key, full width of its column.
static func button(key: String, primary: bool = false) -> Button:
	var b := Button.new()
	b.text = key
	b.custom_minimum_size = Vector2(0.0, PRIMARY_H if primary else BUTTON_H)
	b.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	b.alignment = HORIZONTAL_ALIGNMENT_LEFT
	b.focus_mode = Control.FOCUS_ALL
	b.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	return b


## A button as wide as its text, for an action inside a page (not a menu column). Its text is never
## trimmed: a trimmed text gives a shrinking button no width at all. Its text is centred, so one given
## a fixed width (a header's Close, a run's Stop) reads like every other header button.
static func inline_button(key: String) -> Button:
	var b := button(key)
	b.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	b.text_overrun_behavior = TextServer.OVERRUN_NO_TRIMMING
	b.alignment = HORIZONTAL_ALIGNMENT_CENTER
	return b


## An icon-only button from the hand-written set, with the accessible name and tooltip L1724 asks for.
static func icon_button(stem: StringName, name_text: String) -> Button:
	var b := Button.new()
	b.icon = Ui.icon(stem)
	b.accessibility_name = name_text
	b.tooltip_text = name_text
	b.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	b.custom_minimum_size = Vector2(BUTTON_H, BUTTON_H)
	b.focus_mode = Control.FOCUS_ALL
	b.icon_alignment = HORIZONTAL_ALIGNMENT_CENTER
	return b


static func vbox(step: int) -> VBoxContainer:
	var box := VBoxContainer.new()
	Ui.set_separation(box, step)
	return box


static func hbox(step: int) -> HBoxContainer:
	var box := HBoxContainer.new()
	Ui.set_separation(box, step)
	return box


static func clear(container: Node) -> void:
	for child: Node in container.get_children():
		container.remove_child(child)
		child.queue_free()


# --- focus (L1738: explicit neighbours) ----------------------------------------------------------

## Link `controls` top to bottom: Up and Down move along the list and stop at the ends, focus_next
## and focus_previous follow the same order. Hidden or disabled controls are skipped.
static func chain_vertical(controls: Array) -> Array[Control]:
	var live: Array[Control] = []
	for c: Variant in controls:
		if c is Control and is_instance_valid(c) and (c as Control).focus_mode != Control.FOCUS_NONE and (c as Control).visible:
			if c is BaseButton and (c as BaseButton).disabled:
				continue
			live.append(c)
	var n := live.size()
	for i in n:
		var c := live[i]
		var up := live[i - 1] if i > 0 else c
		var down := live[i + 1] if i < n - 1 else c
		c.focus_neighbor_top = c.get_path_to(up)
		c.focus_neighbor_bottom = c.get_path_to(down)
		c.focus_previous = c.get_path_to(live[(i - 1 + n) % n])
		c.focus_next = c.get_path_to(live[(i + 1) % n])
	return live


## Point Left and Right of every control in `controls` at `left` and `right` (null keeps the control).
static func link_sides(controls: Array, left: Control, right: Control) -> void:
	for c: Variant in controls:
		if not c is Control or not is_instance_valid(c):
			continue
		var control := c as Control
		control.focus_neighbor_left = control.get_path_to(left if left != null else control)
		control.focus_neighbor_right = control.get_path_to(right if right != null else control)


## Link a row left to right (Left stops at the first, Right at the last).
static func chain_horizontal(controls: Array) -> void:
	var live: Array[Control] = []
	for c: Variant in controls:
		if c is Control and is_instance_valid(c) and (c as Control).focus_mode != Control.FOCUS_NONE and (c as Control).visible:
			live.append(c)
	for i in live.size():
		var c := live[i]
		c.focus_neighbor_left = c.get_path_to(live[i - 1] if i > 0 else c)
		c.focus_neighbor_right = c.get_path_to(live[i + 1] if i < live.size() - 1 else c)
