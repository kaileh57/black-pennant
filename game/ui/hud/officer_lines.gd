extends PanelContainer
## Officer and ace radio lines: two 40-character lines, speaker named at changes.
## DESIGN.md L1706, L1721. Unit: ui-hud-board.
## Reads and calls: Hud.officer_line, Tx.lines, Settings.get_value("access.officer_line_scale").
##
## Officers and the seven aces speak as text with radio sounds, never voices (copy rule 12): each
## line is at most two lines of 40 characters, broken at word boundaries by `Tx.lines`, and the
## speaker is named when the speaker changes. The line appears in the frame its signal arrives,
## which is no later than its radio cue starts (the cue is already queued on the audio feed).
## - The newest two lines show, newest at the bottom. Each stays for a reading time of
##   3 s plus 60 ms a character, from 4 s to 9 s, then goes at once (no entrance or exit fades).
## - The speaker is named when the line says so (`new_speaker`: a new speaker, or the same one
##   after 30 s of silence), and also when the box was empty or the line above is someone else's,
##   so a line is never shown without knowing who said it.
## - Accessibility floor 10: the text scales from 100% to 200% (`access.officer_line_scale`) and
##   the backing opacity runs from 0 to 100% (`access.officer_line_opacity`); under reduce
##   transparency the backing is opaque. The text keeps its outline at every opacity (rule 18).
## - The box is a polite live region, so a screen reader reads each line.
## `show_line(line)` takes a hand-built `OfficerLineDict` (tests and captures).

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

const MAX_CHARS := 40
const MAX_LINES := 2
## Lines shown at once.
const KEEP := 2
const HOLD_BASE_S := 3.0
const HOLD_PER_CHAR_S := 0.06
const HOLD_MIN_S := 4.0
const HOLD_MAX_S := 9.0
## Outline of text drawn over the 3D view, as the plot labels have it.
const OUTLINE_PX := 4

@onready var _margin: MarginContainer = $Margin
@onready var _column: VBoxContainer = $Margin/Column

## Shown lines, oldest first: `{ "speaker": String, "role": int, "ace": StringName,
## "text": String, "until_ms": int, "named": bool }`.
var _lines: Array[Dictionary] = []
var _timer: Timer
var _speaker_settings: LabelSettings
var _text_settings: LabelSettings
## Room the HUD shell leaves the box (`set_max_width`); -1: no limit.
var _max_width := -1.0


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_margins(_margin, 2)
	Ui.set_separation(_column, 1)
	_timer = Timer.new()
	_timer.one_shot = true
	_timer.timeout.connect(_expire)
	add_child(_timer)
	Ui.changed.connect(_on_ui_changed)
	_restyle()
	var hud := Kit.autoload(&"Hud")
	if hud != null and hud.has_signal(&"officer_line"):
		hud.connect(&"officer_line", show_line)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", func(_s: int) -> void: clear())
	accessibility_name = tr(&"hud_board.lines.access")
	_render()


func _notification(what: int) -> void:
	if what == NOTIFICATION_THEME_CHANGED and is_node_ready():
		_restyle()


## Show one `OfficerLineDict`.
func show_line(line: Dictionary) -> void:
	var text := _Text.resolve(line.get("text", {}))
	if text.strip_edges().is_empty():
		return
	var speaker := _Text.resolve(line.get("speaker", {}))
	var role := Kit.get_int(line, "role", -1)
	var ace := StringName(line.get("ace", &""))
	var wrapped := _wrap(text)
	var held := clampf(HOLD_BASE_S + HOLD_PER_CHAR_S * text.length(), HOLD_MIN_S, HOLD_MAX_S)
	var entry := {
		"speaker": speaker, "role": role, "ace": ace, "text": wrapped,
		"until_ms": Time.get_ticks_msec() + int(held * 1000.0),
		"new_speaker": Kit.get_bool(line, "new_speaker"),
	}
	_lines.append(entry)
	while _lines.size() > KEEP:
		_lines.pop_front()
	_render()


## Forget every line (a new sortie).
func clear() -> void:
	_lines.clear()
	if is_node_ready():
		_render()


func _wrap(text: String) -> String:
	var tx := Kit.autoload(&"Tx")
	if tx != null and tx.has_method(&"lines"):
		var parts: PackedStringArray = tx.call(&"lines", text, MAX_CHARS, MAX_LINES)
		if not parts.is_empty():
			return "\n".join(parts)
	return text.substr(0, MAX_CHARS * MAX_LINES)


static func _same_speaker(a: Dictionary, b: Dictionary) -> bool:
	return String(a.get("speaker", "")) == String(b.get("speaker", "")) and int(a.get("role", -1)) == int(b.get("role", -1)) and StringName(a.get("ace", &"")) == StringName(b.get("ace", &""))


func _expire() -> void:
	var now := Time.get_ticks_msec()
	var kept: Array[Dictionary] = []
	for l: Dictionary in _lines:
		if int(l["until_ms"]) > now:
			kept.append(l)
	_lines = kept
	_render()


func _render() -> void:
	if not is_node_ready():
		return
	for c: Node in _column.get_children():
		_column.remove_child(c)
		c.queue_free()
	var spoken: PackedStringArray = []
	for i in _lines.size():
		var l := _lines[i]
		var named := bool(l.get("new_speaker", false)) or i == 0 or not _same_speaker(_lines[i - 1], l)
		if named and not String(l["speaker"]).is_empty():
			var who := Label.new()
			who.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
			who.mouse_filter = Control.MOUSE_FILTER_IGNORE
			who.text = String(l["speaker"])
			who.label_settings = _speaker_settings
			_column.add_child(who)
		var said := Label.new()
		said.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
		said.mouse_filter = Control.MOUSE_FILTER_IGNORE
		said.text = String(l["text"])
		said.label_settings = _text_settings
		_fit_line(said)
		_column.add_child(said)
		spoken.append(tr(&"hud_board.lines.row_access").format([String(l["speaker"]), String(l["text"]).replace("\n", " ")]))
	visible = not _lines.is_empty()
	accessibility_description = "\n".join(spoken)
	# Wake for the next line to expire.
	_timer.stop()
	if not _lines.is_empty():
		var now := Time.get_ticks_msec()
		var next := 1 << 62
		for l: Dictionary in _lines:
			next = mini(next, int(l["until_ms"]))
		_timer.start(maxf(0.05, (next - now) / 1000.0))


func _on_ui_changed(what: StringName) -> void:
	if what == &"officer_lines" or what == &"transparency" or what == &"palette" or what == &"scale":
		_restyle()


## The widest the box may be (the HUD shell's centre between the docks); -1: no limit.
func set_max_width(width: float) -> void:
	if is_equal_approx(width, _max_width):
		return
	_max_width = width
	if is_node_ready():
		_render()


## A line keeps its own two-line break (copy rule 12) while the box has room; in a narrow centre (a
## large UI scale or officer-line scale) it wraps again inside the room left.
func _fit_line(label: Label) -> void:
	if _max_width < 0.0 or _text_settings == null or _text_settings.font == null:
		return
	var panel := get_theme_stylebox(&"panel")
	var chrome := (panel.get_margin(SIDE_LEFT) + panel.get_margin(SIDE_RIGHT) if panel != null else 0.0) + float(_margin.get_theme_constant(&"margin_left") + _margin.get_theme_constant(&"margin_right"))
	var room := maxf(_max_width - chrome, 80.0)
	var widest := 0.0
	for part: String in label.text.split("\n"):
		widest = maxf(widest, _text_settings.font.get_string_size(part, HORIZONTAL_ALIGNMENT_LEFT, -1, _text_settings.font_size).x)
	if widest + float(_text_settings.outline_size) <= room:
		return
	label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	label.custom_minimum_size = Vector2(room, 0.0)


## Text size from `access.officer_line_scale` (Body at 100% to 200%) with an outline; the backing
## takes `access.officer_line_opacity`, and only the backing fades.
func _restyle() -> void:
	var s := clampf(Ui.officer_line_scale, 1.0, 2.0)
	var size := roundi(Ui.font_size(&"Body") * s)
	var line_spacing := float(get_theme_constant(&"line_spacing", &"Body")) * s
	_speaker_settings = LabelSettings.new()
	_speaker_settings.font = get_theme_font(&"font", &"PanelTitle")
	_speaker_settings.font_size = size
	_speaker_settings.font_color = Ui.colour(&"text.secondary")
	_speaker_settings.line_spacing = line_spacing
	_speaker_settings.outline_size = roundi(OUTLINE_PX * s)
	_speaker_settings.outline_color = Ui.colour(&"surface.inset")
	_text_settings = LabelSettings.new()
	_text_settings.font = get_theme_font(&"font", &"Body")
	_text_settings.font_size = size
	_text_settings.font_color = Ui.colour(&"text.primary")
	_text_settings.line_spacing = line_spacing
	_text_settings.outline_size = roundi(OUTLINE_PX * s)
	_text_settings.outline_color = Ui.colour(&"surface.inset")
	self_modulate.a = 1.0 if Ui.reduce_transparency else clampf(Ui.officer_line_opacity, 0.0, 1.0)
	if is_node_ready():
		_render()
