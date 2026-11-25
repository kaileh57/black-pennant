@tool
extends RefCounted
## Compiles res://ui/theme/tokens.json into the project themes. DESIGN.md L1620-1651 (tokens),
## L1727-1734 (implementation 1-8), L1742 (every default-theme item is replaced). Unit: ui-theme.
##
## Run it headless from game/:  godot --headless --path . -s res://ui/theme/compile_theme.gd
## (add `-- --check` to fail when the committed themes are stale). It writes:
##   res://ui/theme/project_theme.tres                dusk palette (gui/theme/custom)
##   res://ui/theme/project_theme_high_contrast.tres  high-contrast palette
##   res://ui/fonts/AtkinsonHyperlegibleNext-Variable.msdf.res  MSDF copy for plot and chart labels
## Item names and the token behind each one are listed in theme_tokens.md.
##
## The compiler walks ThemeDB.get_default_theme() and gives every item of every type a value
## derived from the tokens. An item it has no rule for stops the build, so an engine upgrade that
## adds theme items is noticed rather than shipping Godot's look.

const TOKENS_PATH := "res://ui/theme/tokens.json"
const ICON_DIR := "res://ui/icons"
const OUTPUTS: Dictionary[String, String] = {
	"dusk": "res://ui/theme/project_theme.tres",
	"high_contrast": "res://ui/theme/project_theme_high_contrast.tres",
}
## Base resolution of the default theme's metrics against ours (1920x1080 with 22 px body text).
const DEFAULT_TO_PROJECT := 1.5

const LABEL_VARIATIONS: Array[String] = [
	"ScreenTitle", "PanelTitle", "MenuTitle", "Body", "Dense", "Meta", "Code", "PlotLabel",
	"ExpressionText", "Traffic", "Severity", "Stance", "Damage",
]
## Spec names that Godot cannot use for a variation, and the name the theme uses instead.
## DESIGN.md L1729 names "Expression", but Expression is an engine class: a Theme refuses it as
## a variation and Controls then ignore the type, so the look is registered as ExpressionText.
const VARIATION_ALIASES: Dictionary[String, String] = {"Expression": "ExpressionText"}
const TITLE_VARIATIONS: Array[String] = ["ScreenTitle", "PanelTitle", "MenuTitle"]
const CUSTOM_TYPES: Array[String] = ["Plot", "Chart", "Gauge"]
## Row variations with a single-side stripe (L1596), one sub-variation per state.
const STRIPE_STATES: Dictionary[String, Array] = {
	"Stance": ["Friend", "Neutral", "Suspect", "Hostile"],
	"Severity": ["Pass", "Caution", "Fail", "Advisory"],
	"Damage": ["Intact", "Damaged", "Heavy", "Destroyed", "Flooding"],
}
const STRIPE_PX := 4

var tokens: Dictionary = {}
var errors: PackedStringArray = []

var _palette_name := ""
var _palette: Dictionary = {}
var _chart: Dictionary = {}
var _hc := false
var _theme: Theme
var _ids: Dictionary[String, int] = {}
var _icon_cache: Dictionary[String, Texture2D] = {}
var _svg_cache: Dictionary[String, String] = {}
var _fonts: Dictionary[String, Font] = {}
var _empty_texture: ImageTexture


func _init() -> void:
	var text := FileAccess.get_file_as_string(TOKENS_PATH)
	var parsed: Variant = JSON.parse_string(text)
	if parsed is Dictionary:
		tokens = parsed
	else:
		errors.append("cannot parse " + TOKENS_PATH)


# --- public ------------------------------------------------------------------------------------

## Palettes this compiler writes, in output order.
func palettes() -> Array[String]:
	var out: Array[String] = []
	for key: String in OUTPUTS:
		out.append(key)
	return out


## font_size for a body height in px at 1080p: ceil(height / ratio) (L1647, L1734).
static func font_size_for(height_px: float, ratio: float) -> int:
	return ceili(height_px / ratio - 0.0001)


## A token hex string as a colour; the one place outside the theme files that needs a colour
## from tokens.json reads it through here.
static func hex(text: String) -> Color:
	return Color.html(text) if Color.html_is_valid(text) else Color(0, 0, 0, 1)


## WCAG 2 contrast ratio of two opaque colours.
static func contrast(a: Color, b: Color) -> float:
	var la := _rel_luminance(a)
	var lb := _rel_luminance(b)
	return (maxf(la, lb) + 0.05) / (minf(la, lb) + 0.05)


## Build the theme for one palette. Check `errors` afterwards.
func build(palette_name: String) -> Theme:
	_palette_name = palette_name
	_hc = palette_name == "high_contrast"
	var palettes_dict: Dictionary = tokens.get("palettes", {})
	_palette = palettes_dict.get(palette_name, {})
	_chart = palettes_dict.get("chart", {})
	if _palette.is_empty():
		errors.append("no palette " + palette_name)
		return null
	_theme = Theme.new()
	_ids.clear()
	_icon_cache.clear()
	_fonts.clear()
	_empty_texture = null
	_build_fonts()
	if not errors.is_empty():
		return null
	_theme.default_font = _fonts["sans"]
	_theme.default_font_size = _type_size("Body")
	_theme.default_base_scale = 1.0
	_build_defaults()
	_build_label_variations()
	_build_stripe_variations()
	_build_custom_types()
	return _theme


## Build every palette and write it. Returns false on any error.
func write_all() -> bool:
	if not errors.is_empty():
		return false
	if not _write_msdf_font():
		return false
	for palette_name: String in OUTPUTS:
		var theme := build(palette_name)
		if theme == null or not errors.is_empty():
			return false
		var text := serialize(theme)
		if text.is_empty():
			return false
		var file := FileAccess.open(OUTPUTS[palette_name], FileAccess.WRITE)
		if file == null:
			errors.append("cannot write " + OUTPUTS[palette_name])
			return false
		file.store_string(text)
		file.close()
	return true


## Compare freshly built themes with the committed files. Returns the stale paths.
func stale_outputs() -> PackedStringArray:
	var stale: PackedStringArray = []
	for palette_name: String in OUTPUTS:
		var theme := build(palette_name)
		if theme == null:
			stale.append(OUTPUTS[palette_name])
			continue
		var fresh := serialize(theme).replace("\r", "")
		# A checkout with CRLF line ends still compares equal.
		var committed := FileAccess.get_file_as_string(OUTPUTS[palette_name]).replace("\r", "")
		if fresh != committed:
			stale.append(OUTPUTS[palette_name])
	return stale


## Deterministic .tres text: Godot gives external resources random ids, so they are renamed in
## order of appearance.
func serialize(theme: Theme) -> String:
	var tmp := "user://theme_compiler_tmp.tres"
	var err := ResourceSaver.save(theme, tmp)
	if err != OK:
		errors.append("cannot save theme: %d" % err)
		return ""
	var text := FileAccess.get_file_as_string(tmp)
	DirAccess.remove_absolute(ProjectSettings.globalize_path(tmp))
	var regex := RegEx.create_from_string("\\[ext_resource [^\\]]*id=\"([^\"]+)\"\\]")
	var n := 0
	for m: RegExMatch in regex.search_all(text):
		n += 1
		var old_id := m.get_string(1)
		var new_id := "%d_ext" % n
		text = text.replace("id=\"%s\"]" % old_id, "id=\"%s\"]" % new_id)
		text = text.replace("ExtResource(\"%s\")" % old_id, "ExtResource(\"%s\")" % new_id)
	return text


# --- colours -----------------------------------------------------------------------------------

## A palette colour by token name ("text.primary"), or a chart colour ("chart.ink").
func colour(token: String) -> Color:
	if token.begins_with("chart."):
		var key := token.substr(6)
		if not _chart.has(key):
			errors.append("no chart token " + key)
			return Color()
		return Color.html(_chart[key])
	if not _palette.has(token):
		errors.append("no token %s in palette %s" % [token, _palette_name])
		return Color()
	return Color.html(_palette[token])


func _clear(token: String) -> Color:
	var c := colour(token)
	c.a = 0.0
	return c


func _alpha(token: String, a: float) -> Color:
	var c := colour(token)
	c.a = a
	return c


static func _rel_luminance(c: Color) -> float:
	var lin := c.srgb_to_linear()
	return 0.2126 * lin.r + 0.7152 * lin.g + 0.0722 * lin.b


# Item name -> token for every colour in the default theme, then per-type exceptions.
const COLOUR_BY_NAME: Dictionary[String, String] = {
	"font_color": "text.primary",
	"font_pressed_color": "text.primary",
	"font_hover_color": "text.primary",
	"font_focus_color": "text.primary",
	"font_hover_pressed_color": "text.primary",
	"font_hovered_color": "text.primary",
	"font_hovered_selected_color": "text.primary",
	"font_selected_color": "text.primary",
	"font_hovered_dimmed_color": "text.secondary",
	"font_unselected_color": "text.secondary",
	"font_disabled_color": "text.inactive",
	"font_readonly_color": "text.inactive",
	"font_uneditable_color": "text.inactive",
	"font_placeholder_color": "text.secondary",
	"font_accelerator_color": "text.secondary",
	"font_separator_color": "text.secondary",
	"font_outline_color": "surface.inset",
	"font_separator_outline_color": "surface.inset",
	"font_shadow_color": "clear:surface.inset",
	"default_color": "text.primary",
	"hover_font_color": "text.primary",
	"collapsed_font_color": "text.primary",
	"title_color": "text.primary",
	"title_outline_modulate": "surface.inset",
	"title_button_color": "text.primary",
	"custom_button_font_highlight": "text.primary",
	"icon_normal_color": "text.primary",
	"icon_pressed_color": "text.primary",
	"icon_hover_color": "text.primary",
	"icon_hover_pressed_color": "text.primary",
	"icon_focus_color": "text.primary",
	"icon_disabled_color": "text.inactive",
	"icon_selected_color": "text.primary",
	"icon_hovered_color": "text.primary",
	"icon_unselected_color": "text.secondary",
	"checkbox_checked_color": "text.primary",
	"checkbox_unchecked_color": "text.secondary",
	"button_checked_color": "text.primary",
	"button_unchecked_color": "text.secondary",
	"caret_color": "text.primary",
	"caret_background_color": "surface.inset",
	"selection_color": "sel_bg",
	"background_color": "clear:surface.inset",
	"current_line_color": "surface.raised",
	"word_highlighted_color": "rule.divider",
	"search_result_color": "rule.divider",
	"search_result_border_color": "text.secondary",
	"guide_color": "rule.divider",
	"relationship_line_color": "rule.divider",
	"parent_hl_line_color": "text.secondary",
	"children_hl_line_color": "text.secondary",
	"drop_on_item_color": "ui.box",
	"drop_position_color": "ui.box",
	"drop_mark_color": "ui.box",
	"scroll_hint_color": "surface.inset",
	"scroll_hint_vertical_color": "surface.inset",
	"scroll_hint_horizontal_color": "surface.inset",
	"clear_button_color": "text.secondary",
	"clear_button_color_pressed": "text.primary",
	"touch_dragger_color": "text.inactive",
	"touch_dragger_hover_color": "text.secondary",
	"touch_dragger_pressed_color": "text.primary",
	"resizer_color": "text.secondary",
	"table_odd_row_bg": "clear:surface.base",
	"table_even_row_bg": "surface.raised",
	"table_border": "rule.divider",
	"folder_icon_color": "text.secondary",
	"file_icon_color": "text.secondary",
	"file_disabled_color": "text.inactive",
	"focused_not_editing_cursor_color": "ui.focus",
	"up_icon_modulate": "text.secondary",
	"up_hover_icon_modulate": "text.primary",
	"up_pressed_icon_modulate": "text.primary",
	"up_disabled_icon_modulate": "text.inactive",
	"down_icon_modulate": "text.secondary",
	"down_hover_icon_modulate": "text.primary",
	"down_pressed_icon_modulate": "text.primary",
	"down_disabled_icon_modulate": "text.inactive",
	"completion_background_color": "surface.raised",
	"completion_selected_color": "rule.divider",
	"completion_existing_color": "text.inactive",
	"completion_scroll_color": "rule.divider",
	"completion_scroll_hovered_color": "text.inactive",
	"bookmark_color": "text.secondary",
	"breakpoint_color": "text.primary",
	"executing_line_color": "text.primary",
	"code_folding_color": "text.secondary",
	"folded_code_region_color": "rule.divider",
	"brace_mismatch_color": "alert.caution",
	"line_number_color": "text.inactive",
	"line_length_guideline_color": "rule.divider",
	"grid_minor": "surface.raised",
	"grid_major": "rule.divider",
	"selection_fill": "alpha15:text.primary",
	"selection_stroke": "select",
	"activity": "text.primary",
	"connection_hover_tint_color": "alpha30:surface.inset",
	"connection_valid_target_tint_color": "alpha40:text.primary",
	"connection_rim_color": "surface.inset",
}
const COLOUR_BY_TYPE: Dictionary[String, String] = {
	# TextEdit and CodeEdit treat a clear selected-font colour as "keep the syntax colour".
	"TextEdit/font_selected_color": "sel_fg_keep",
	"CodeEdit/font_selected_color": "sel_fg_keep",
	"RichTextLabel/font_selected_color": "sel_fg_keep",
	"LineEdit/font_selected_color": "sel_fg",
	"TabBar/font_selected_color": "text.primary",
	"TabContainer/font_selected_color": "text.primary",
	"PopupMenu/font_hover_color": "sel_fg",
	"FoldableContainer/font_outline_color": "surface.inset",
}


func _colour_rule(rule: String) -> Color:
	match rule:
		"sel_bg":
			return _selection_colours()[0]
		"sel_fg":
			return _selection_colours()[1]
		"sel_fg_keep":
			# Dusk keeps the text's own colour on a selection (clear = keep); high contrast inverts.
			return _clear("surface.inset") if not _selection_inverted() else _selection_colours()[1]
	if rule.begins_with("clear:"):
		return _clear(rule.substr(6))
	if rule.begins_with("alpha"):
		var colon := rule.find(":")
		var pct := rule.substr(5, colon - 5).to_int()
		return _alpha(rule.substr(colon + 1), pct / 100.0)
	return colour(rule)


## Selected rows and text: rule.divider under text.primary while that pair meets the palette's
## text floor (4.5:1, 7:1 in high contrast, L1714); otherwise an inverse text.secondary fill
## under surface.inset text.
func _selection_inverted() -> bool:
	var floor_ratio := 7.0 if _hc else 4.5
	return contrast(colour("text.primary"), colour("rule.divider")) < floor_ratio


func _selection_colours() -> Array[Color]:
	if _selection_inverted():
		return [colour("text.secondary"), colour("surface.inset")]
	return [colour("rule.divider"), colour("text.primary")]


func _default_colour(type: String, item: String) -> Color:
	var key := type + "/" + item
	if COLOUR_BY_TYPE.has(key):
		return _colour_rule(COLOUR_BY_TYPE[key])
	if COLOUR_BY_NAME.has(item):
		return _colour_rule(COLOUR_BY_NAME[item])
	errors.append("no colour rule for %s" % key)
	return Color()


# --- fonts -------------------------------------------------------------------------------------

func _font_tokens(key: String) -> Dictionary:
	var fonts: Dictionary = tokens.get("fonts", {})
	return fonts.get(key, {})


func _type_tokens(variation: String) -> Dictionary:
	var types: Dictionary = tokens.get("type", {})
	return types.get(variation, {})


## font_size of a type variation, derived from its body height and the font's ratio (L1734).
func _type_size(variation: String) -> int:
	var t := _type_tokens(variation)
	var family: String = t.get("font", "sans")
	var ratio: float = _font_tokens(family).get("height_ratio", 0.91)
	var size := font_size_for(float(t.get("height_px", 20)), ratio)
	if t.has("font_size") and int(t["font_size"]) != size:
		errors.append("%s: tokens give font_size %d but height %s / %s gives %d" % [variation, int(t["font_size"]), str(t["height_px"]), str(ratio), size])
	return size


func _tag(name: String) -> int:
	return TextServerManager.get_primary_interface().name_to_tag(name)


func _build_fonts() -> void:
	var sans_tokens := _font_tokens("sans")
	var sans_path: String = sans_tokens.get("file", "")
	var sans_file := load(sans_path) as FontFile if ResourceLoader.exists(sans_path) else null
	if sans_file == null:
		errors.append("missing sans font " + sans_path)
		return
	var fallbacks := _fallback_fonts()
	var weights: Array = sans_tokens.get("weights", [400, 700])
	_fonts["sans"] = _variation("font_sans_regular", sans_file, int(weights[0]), true, 0, fallbacks)
	_fonts["sans_bold"] = _variation("font_sans_bold", sans_file, int(weights[1]), true, 0, fallbacks)
	_fonts["code"] = _variation("font_code", sans_file, int(weights[0]), true, int(_type_tokens("Code").get("spacing_glyph", 1)), fallbacks)
	var msdf_path: String = sans_tokens.get("msdf_file", "")
	var msdf_file: FontFile = load(msdf_path) as FontFile if ResourceLoader.exists(msdf_path) else null
	if msdf_file == null:
		# First run: the MSDF copy is written before the themes, see write_all().
		msdf_file = _make_msdf_font()
	_fonts["plot"] = _variation("font_plot", msdf_file, int(weights[0]), true, 0, fallbacks)
	_fonts["plot_bold"] = _variation("font_plot_bold", msdf_file, int(weights[1]), true, 0, fallbacks)
	var mono_tokens := _font_tokens("mono")
	var mono_path: String = mono_tokens.get("file", "")
	if ResourceLoader.exists(mono_path):
		var mono_file := load(mono_path) as FontFile
		var mono_weights: Array = mono_tokens.get("weights", [400, 700])
		_fonts["mono"] = _variation("font_mono_regular", mono_file, int(mono_weights[0]), false, 0, fallbacks)
		_fonts["mono_bold"] = _variation("font_mono_bold", mono_file, int(mono_weights[1]), false, 0, fallbacks)
	else:
		# Atkinson Hyperlegible Mono is not in the repository yet: ExpressionText and Traffic use the
		# sans with tabular figures until it is added at mono.file (then recompile).
		push_warning("theme: %s missing; ExpressionText and Traffic fall back to the sans" % mono_path)
		_fonts["mono"] = _fonts["sans"]
		_fonts["mono_bold"] = _fonts["sans_bold"]


## OFL fallbacks for locales Atkinson does not cover (L1645), when their files are present.
## Each is marked as supporting its language so Han glyphs pick the right face.
func _fallback_fonts() -> Array[Font]:
	var out: Array[Font] = []
	var list: Dictionary = _font_tokens("fallbacks")
	for locale: String in list:
		var entry: Dictionary = list[locale]
		var path: String = entry.get("file", "")
		if not ResourceLoader.exists(path):
			continue
		var font := load(path) as FontFile
		if font == null:
			continue
		var language := locale.split("_")[0]
		font.set_language_support_override(language, true)
		out.append(font)
	return out


func _variation(id: String, base: Font, wght: int, tnum: bool, spacing_glyph: int, fallbacks: Array[Font]) -> FontVariation:
	var fv := FontVariation.new()
	fv.resource_scene_unique_id = id
	fv.base_font = base
	fv.variation_opentype = {_tag("wght"): wght}
	if tnum:
		fv.opentype_features = {_tag("tnum"): 1}
	if spacing_glyph != 0:
		fv.spacing_glyph = spacing_glyph
	if not fallbacks.is_empty():
		fv.fallbacks = fallbacks
	return fv


func _make_msdf_font() -> FontFile:
	var sans_tokens := _font_tokens("sans")
	var font := FontFile.new()
	font.data = FileAccess.get_file_as_bytes(sans_tokens.get("file", ""))
	font.multichannel_signed_distance_field = true
	font.msdf_pixel_range = int(sans_tokens.get("msdf_pixel_range", 16))
	font.msdf_size = int(sans_tokens.get("msdf_size", 48))
	font.generate_mipmaps = true
	font.hinting = TextServer.HINTING_NONE
	return font


func _write_msdf_font() -> bool:
	var path: String = _font_tokens("sans").get("msdf_file", "")
	var outline: int = int(_stroke("label_outline"))
	var font := _make_msdf_font()
	if font.msdf_pixel_range < 2 * outline:
		errors.append("msdf_pixel_range %d is under twice the label outline %d (L1733)" % [font.msdf_pixel_range, outline])
		return false
	var err := ResourceSaver.save(font, path)
	if err != OK:
		errors.append("cannot write %s: %d" % [path, err])
		return false
	return true


# --- style boxes -------------------------------------------------------------------------------

func _next_id(prefix: String) -> String:
	var n: int = _ids.get(prefix, 0) + 1
	_ids[prefix] = n
	return "%s_%d" % [prefix, n]


func _radius(key: String) -> int:
	var r: Dictionary = tokens.get("radius", {})
	return mini(int(r.get(key, 0)), int(r.get("max", 4)))


func _stroke(key: String) -> int:
	var s: Dictionary = tokens.get("stroke", {})
	return int(s.get(key, 1))


func _space(index: int) -> int:
	var steps: Array = tokens.get("spacing", [4, 8, 12, 16, 24, 32, 48])
	return int(steps[clampi(index, 0, steps.size() - 1)])


func _flat(bg: Variant, border: Variant, border_px: int, radius: int, margin_h: int, margin_v: int) -> StyleBoxFlat:
	var sb := StyleBoxFlat.new()
	sb.resource_scene_unique_id = _next_id("sb")
	if bg == null:
		sb.draw_center = false
		sb.bg_color = _clear("surface.base")
	else:
		sb.bg_color = bg
	if border != null and border_px > 0:
		sb.border_color = border
		sb.set_border_width_all(border_px)
	sb.set_corner_radius_all(radius)
	sb.corner_detail = 4
	sb.anti_aliasing = radius > 0
	sb.shadow_size = 0
	sb.content_margin_left = margin_h
	sb.content_margin_right = margin_h
	sb.content_margin_top = margin_v
	sb.content_margin_bottom = margin_v
	return sb


func _empty(margin_h: float, margin_v: float) -> StyleBoxEmpty:
	var sb := StyleBoxEmpty.new()
	sb.resource_scene_unique_id = _next_id("sbe")
	sb.content_margin_left = margin_h
	sb.content_margin_right = margin_h
	sb.content_margin_top = margin_v
	sb.content_margin_bottom = margin_v
	return sb


func _line(vertical: bool) -> StyleBoxLine:
	var sb := StyleBoxLine.new()
	sb.resource_scene_unique_id = _next_id("sbl")
	sb.color = colour("rule.divider")
	sb.thickness = _stroke("rule")
	sb.vertical = vertical
	sb.grow_begin = 0.0
	sb.grow_end = 0.0
	return sb


## Focus ring: 2 px ui.focus outside the control, no fill (L1716, L1738).
func _focus(radius: int) -> StyleBoxFlat:
	var w := _stroke("focus")
	var sb := _flat(null, colour("ui.focus"), w, radius + (w if radius > 0 else 0), 0, 0)
	sb.set_expand_margin_all(w)
	return sb


## Control style boxes by state: buttons and anything pressable (radius 2, L1565).
func _control(state: String, margin_h: int, margin_v: int) -> StyleBox:
	var r := _radius("control")
	var rule := _stroke("rule")
	match state:
		"normal", "normal_mirrored":
			return _flat(colour("surface.raised"), colour("rule.divider"), rule, r, margin_h, margin_v)
		"hover", "hover_mirrored":
			return _flat(colour("surface.raised"), colour("text.secondary"), rule, r, margin_h, margin_v)
		"pressed", "pressed_mirrored", "hover_pressed", "hover_pressed_mirrored":
			return _flat(colour("surface.inset"), colour("text.primary"), rule, r, margin_h, margin_v)
		"disabled", "disabled_mirrored":
			return _flat(colour("surface.base"), colour("rule.divider"), rule, r, margin_h, margin_v)
		"focus":
			return _focus(r)
	errors.append("no control state " + state)
	return _empty(margin_h, margin_v)


## Text fields: inset well, 1 px rule (L1565).
func _field(state: String) -> StyleBox:
	var r := _radius("control")
	match state:
		"normal":
			return _flat(colour("surface.inset"), colour("rule.divider"), _stroke("rule"), r, _space(1), _space(0) + 2)
		"read_only":
			return _flat(colour("surface.base"), colour("rule.divider"), _stroke("rule"), r, _space(1), _space(0) + 2)
		"focus":
			return _focus(r)
	errors.append("no field state " + state)
	return _empty(0, 0)


## Lists and trees: inset well, selection by value and a frame, never hue (L1658).
func _list(item: String) -> StyleBox:
	var focus_w := _stroke("focus")
	match item:
		"panel":
			return _flat(colour("surface.inset"), colour("rule.divider"), _stroke("rule"), _radius("frame"), _space(0), _space(0))
		"focus":
			return _focus(_radius("frame"))
		"hovered", "hovered_dimmed", "button_hover", "custom_button_hover", "title_button_hover":
			return _flat(colour("surface.raised"), null, 0, 0, _space(0), _space(0) / 2)
		"selected", "hovered_selected", "button_pressed", "custom_button_pressed", "title_button_pressed", "selected_focus", "hovered_selected_focus":
			# Lists draw selected text in font_color, so the row keeps text.primary legible: a
			# rule.divider fill where that pair meets the text floor, else a 3 px select frame.
			if not _selection_inverted():
				return _flat(colour("rule.divider"), null, 0, 0, _space(0), _space(0) / 2)
			return _flat(colour("surface.raised"), colour("select"), _stroke("selection"), 0, _space(0), _space(0) / 2)
		"cursor":
			return _flat(null, colour("ui.focus"), focus_w, 0, 0, 0)
		"cursor_unfocused":
			return _flat(null, colour("text.secondary"), _stroke("rule"), 0, 0, 0)
		"custom_button", "title_button_normal":
			return _flat(colour("surface.raised"), colour("rule.divider"), _stroke("rule"), _radius("control"), _space(0), _space(0) / 2)
	errors.append("no list item " + item)
	return _empty(0, 0)


## Docked panel: opaque surface.base, radius 0, no shadow (L1565, L1594).
func _panel() -> StyleBoxFlat:
	return _flat(colour("surface.base"), null, 0, _radius("frame"), _space(3), _space(3))


## Popups and dialogs: raised surface with a 1 px rule.
func _popup() -> StyleBoxFlat:
	return _flat(colour("surface.raised"), colour("rule.divider"), _stroke("rule"), _radius("frame"), _space(2), _space(2))


func _default_stylebox(type: String, item: String, original: StyleBox) -> StyleBox:
	var key := type + "/" + item
	match key:
		"Label/normal", "TooltipLabel/normal", "GraphNodeTitleLabel/normal", "GraphFrameTitleLabel/normal", "RichTextLabel/normal", "ScrollContainer/panel", "HSplitContainer/split_bar_background", "VSplitContainer/split_bar_background", "SplitContainer/split_bar_background":
			return _empty(0, 0)
		"Label/focus", "RichTextLabel/focus", "ScrollContainer/focus", "LinkButton/focus", "ColorPicker/sample_focus", "ColorPresetButton/preset_focus", "FoldableContainer/focus", "ItemList/focus", "Tree/focus", "TabBar/tab_focus", "TabContainer/tab_focus", "HScrollBar/scroll_focus", "VScrollBar/scroll_focus":
			return _focus(_radius("frame"))
		"ColorPicker/picker_focus_rectangle":
			return _focus(0)
		"ColorPicker/picker_focus_circle":
			# Radii stop at radius.max (L1565), so the wheel's focus is the 2 px square ring.
			return _focus(0)
		"ColorPresetButton/preset_fg":
			return _flat(null, colour("rule.divider"), _stroke("rule"), _radius("control"), 0, 0)
		"Panel/panel", "PanelContainer/panel", "TabContainer/panel":
			return _panel()
		"PopupPanel/panel", "PopupDialog/panel", "AcceptDialog/panel", "TooltipPanel/panel", "PopupMenu/panel":
			var p := _popup()
			if type == "TooltipPanel":
				p.set_content_margin_all(_space(1))
			if type == "PopupMenu":
				p.set_content_margin_all(_space(0))
			return p
		"PopupMenu/hover":
			return _flat(_selection_colours()[0], null, 0, 0, _space(1), _space(0))
		"PopupMenu/separator", "PopupMenu/labeled_separator_left", "PopupMenu/labeled_separator_right", "HSeparator/separator":
			return _line(false)
		"VSeparator/separator":
			return _line(true)
		"Window/embedded_border", "Window/embedded_unfocused_border":
			var w := _flat(colour("surface.raised"), colour("rule.divider") if item.contains("unfocused") else colour("text.secondary"), _stroke("rule"), _radius("frame"), _space(1), _space(1))
			w.expand_margin_top = 40
			w.set_expand_margin(SIDE_LEFT, _stroke("rule"))
			w.set_expand_margin(SIDE_RIGHT, _stroke("rule"))
			w.set_expand_margin(SIDE_BOTTOM, _stroke("rule"))
			return w
		"LineEdit/normal", "TextEdit/normal", "CodeEdit/normal":
			return _field("normal")
		"LineEdit/read_only", "TextEdit/read_only", "CodeEdit/read_only":
			return _field("read_only")
		"LineEdit/focus", "TextEdit/focus", "CodeEdit/focus":
			return _field("focus")
		"CodeEdit/completion":
			return _popup()
		"HScrollBar/scroll", "VScrollBar/scroll":
			return _flat(colour("surface.inset"), null, 0, 0, 4, 4)
		"HScrollBar/grabber", "VScrollBar/grabber":
			return _flat(colour("rule.divider"), null, 0, _radius("control"), 4, 4)
		"HScrollBar/grabber_highlight", "VScrollBar/grabber_highlight":
			return _flat(colour("text.inactive"), null, 0, _radius("control"), 4, 4)
		"HScrollBar/grabber_pressed", "VScrollBar/grabber_pressed":
			return _flat(colour("text.secondary"), null, 0, _radius("control"), 4, 4)
		"HSlider/slider", "VSlider/slider":
			return _flat(colour("rule.divider"), null, 0, _radius("control"), 2, 2)
		"HSlider/grabber_area", "VSlider/grabber_area":
			return _flat(colour("text.secondary"), null, 0, _radius("control"), 2, 2)
		"HSlider/grabber_area_highlight", "VSlider/grabber_area_highlight":
			return _flat(colour("text.primary"), null, 0, _radius("control"), 2, 2)
		"ProgressBar/background":
			return _flat(colour("surface.inset"), colour("rule.divider"), _stroke("rule"), _radius("frame"), 2, 2)
		"ProgressBar/fill":
			return _flat(colour("text.secondary"), null, 0, _radius("frame"), 2, 2)
		"TabBar/tab_selected", "TabContainer/tab_selected":
			var sel := _flat(colour("surface.base"), colour("rule.divider"), _stroke("rule"), 0, _space(3), _space(1))
			sel.border_width_bottom = 0
			return sel
		"TabBar/tab_hovered", "TabContainer/tab_hovered":
			return _flat(colour("surface.raised"), null, 0, 0, _space(3), _space(1))
		"TabBar/tab_unselected", "TabContainer/tab_unselected", "TabBar/tab_disabled", "TabContainer/tab_disabled":
			return _flat(colour("surface.inset"), null, 0, 0, _space(3), _space(1))
		"TabBar/button_pressed":
			return _flat(colour("surface.inset"), colour("text.primary"), _stroke("rule"), _radius("control"), 4, 4)
		"TabBar/button_highlight":
			return _flat(colour("surface.raised"), colour("text.secondary"), _stroke("rule"), _radius("control"), 4, 4)
		"TabContainer/tabbar_background":
			return _empty(0, 0)
		"FoldableContainer/title_panel", "FoldableContainer/title_collapsed_panel":
			return _flat(colour("surface.raised"), null, 0, 0, _space(2), _space(1))
		"FoldableContainer/title_hover_panel", "FoldableContainer/title_collapsed_hover_panel":
			return _flat(colour("surface.raised"), colour("text.secondary"), _stroke("rule"), 0, _space(2), _space(1))
		"FoldableContainer/panel":
			return _flat(colour("surface.base"), colour("rule.divider"), _stroke("rule"), 0, _space(2), _space(2))
		"GraphEdit/panel", "GraphEdit/panel_focus":
			var g := _flat(colour("surface.inset"), null, 0, 0, 0, 0)
			if item == "panel_focus":
				g.border_color = colour("ui.focus")
				g.set_border_width_all(_stroke("focus"))
			return g
		"GraphEdit/menu_panel":
			return _popup()
		"GraphEditMinimap/panel":
			return _flat(colour("surface.base"), colour("rule.divider"), _stroke("rule"), 0, 0, 0)
		"GraphEditMinimap/camera":
			return _flat(null, colour("text.secondary"), _stroke("rule"), 0, 0, 0)
		"GraphEditMinimap/node":
			return _flat(colour("rule.divider"), null, 0, 0, 0, 0)
		"GraphNode/panel", "GraphNode/panel_selected", "GraphNode/panel_focus":
			var tile := _radius("tile")
			var n := _flat(colour("surface.raised"), colour("rule.divider"), _stroke("rule"), tile, _space(2), _space(1))
			n.corner_radius_top_left = 0
			n.corner_radius_top_right = 0
			if item == "panel_selected":
				n.border_color = colour("select")
				n.set_border_width_all(_stroke("selection"))
			elif item == "panel_focus":
				n.border_color = colour("ui.focus")
				n.set_border_width_all(_stroke("focus"))
			return n
		"GraphNode/titlebar", "GraphNode/titlebar_selected":
			var t := _flat(colour("surface.base"), colour("rule.divider"), _stroke("rule"), _radius("tile"), _space(2), _space(1))
			t.corner_radius_bottom_left = 0
			t.corner_radius_bottom_right = 0
			if item == "titlebar_selected":
				t.border_color = colour("select")
				t.set_border_width_all(_stroke("selection"))
			return t
		"GraphNode/slot":
			return _empty(0, 0)
		"GraphNode/slot_selected":
			return _flat(null, colour("text.secondary"), _stroke("rule"), 0, 0, 0)
		"GraphFrame/panel", "GraphFrame/panel_selected":
			var fr := _flat(colour("surface.base"), colour("rule.divider"), _stroke("rule"), _radius("tile"), _space(2), _space(2))
			if item == "panel_selected":
				fr.border_color = colour("select")
				fr.set_border_width_all(_stroke("selection"))
			return fr
		"GraphFrame/titlebar", "GraphFrame/titlebar_selected":
			return _empty(_space(2), _space(1))
		"VirtualJoystick/normal_joystick", "VirtualJoystick/pressed_joystick":
			var j := _flat(null, colour("text.secondary") if item.begins_with("normal") else colour("text.primary"), _stroke("frame"), _radius("max"), 0, 0)
			j.anti_aliasing = true
			return j
		"VirtualJoystick/normal_tip", "VirtualJoystick/pressed_tip":
			var tip := _flat(colour("text.secondary") if item.begins_with("normal") else colour("text.primary"), null, 0, _radius("max"), 0, 0)
			tip.anti_aliasing = true
			return tip
		"SpinBox/up_background", "SpinBox/down_background", "SpinBox/up_background_disabled", "SpinBox/down_background_disabled", "SpinBox/field_and_buttons_separator", "SpinBox/up_down_buttons_separator":
			return _empty(2, 0)
		"SpinBox/up_background_hovered", "SpinBox/down_background_hovered":
			return _flat(colour("surface.raised"), null, 0, 0, 2, 0)
		"SpinBox/up_background_pressed", "SpinBox/down_background_pressed":
			return _flat(colour("surface.inset"), null, 0, 0, 2, 0)
	if type in ["ItemList", "Tree"]:
		return _list(item)
	if type in ["Button", "MenuButton", "OptionButton", "ColorPickerButton", "MenuBar", "FlatButton", "FlatMenuButton", "CheckBox", "CheckButton"]:
		if original is StyleBoxEmpty and item != "focus":
			return _empty(_space(0), _space(0))
		if type in ["FlatButton", "FlatMenuButton"] and item == "pressed":
			return _flat(colour("surface.inset"), null, 0, _radius("control"), _space(2), _space(1))
		return _control(item, _space(2), _space(1))
	errors.append("no style rule for " + key)
	return _empty(0, 0)


# --- icons -------------------------------------------------------------------------------------

## Default-theme icon -> [icon stem in res://ui/icons, colour token, scale or 0 for derived].
## "" as the stem means an empty texture (the control draws nothing there).
const ICON_BY_NAME: Dictionary[String, Array] = {
	"checked": ["box_checked", "text.primary", 0],
	"checked_disabled": ["box_checked", "text.inactive", 0],
	"unchecked": ["box_unchecked", "text.secondary", 0],
	"unchecked_disabled": ["box_unchecked", "text.inactive", 0],
	"indeterminate": ["box_mixed", "text.primary", 0],
	"indeterminate_disabled": ["box_mixed", "text.inactive", 0],
	"radio_checked": ["radio_checked", "text.primary", 0],
	"radio_checked_disabled": ["radio_checked", "text.inactive", 0],
	"radio_unchecked": ["radio_unchecked", "text.secondary", 0],
	"radio_unchecked_disabled": ["radio_unchecked", "text.inactive", 0],
	"submenu": ["chevron_right", "text.secondary", 0],
	"submenu_mirrored": ["chevron_left", "text.secondary", 0],
	"search": ["search", "text.secondary", 0],
	"close": ["close", "text.secondary", 0],
	"close_pressed": ["close", "text.primary", 0],
	"clear": ["close", "text.secondary", 0],
	"arrow": ["chevron_down", "text.secondary", 0],
	"arrow_collapsed": ["chevron_right", "text.secondary", 0],
	"arrow_collapsed_mirrored": ["chevron_left", "text.secondary", 0],
	"select_arrow": ["chevron_down", "text.secondary", 0],
	"updown": ["updown", "text.secondary", 0],
	"expanded_arrow": ["chevron_down", "text.secondary", 0],
	"expanded_arrow_mirrored": ["chevron_down", "text.secondary", 0],
	"folded_arrow": ["chevron_right", "text.secondary", 0],
	"folded_arrow_mirrored": ["chevron_left", "text.secondary", 0],
	"scroll_hint": ["", "", 0],
	"scroll_hint_vertical": ["", "", 0],
	"scroll_hint_horizontal": ["", "", 0],
	"increment": ["chevron_right", "text.secondary", 0],
	"increment_highlight": ["chevron_right", "text.primary", 0],
	"decrement": ["chevron_left", "text.secondary", 0],
	"decrement_highlight": ["chevron_left", "text.primary", 0],
	"drop_mark": ["drop_mark", "ui.box", 0],
	"menu": ["menu", "text.secondary", 0],
	"menu_highlight": ["menu", "text.primary", 0],
	"grabber": ["grabber", "text.primary", 0.75],
	"grabber_highlight": ["grabber", "ui.focus", 0.75],
	"grabber_disabled": ["grabber", "text.inactive", 0.75],
	"tick": ["tick", "text.secondary", 0.5],
	"touch_dragger": ["dragger", "text.secondary", 1.0],
	"h_touch_dragger": ["dragger", "text.secondary", 1.0],
	"v_touch_dragger": ["dragger_horizontal", "text.secondary", 1.0],
	"h_grabber": ["dragger", "text.secondary", 1.0],
	"v_grabber": ["dragger_horizontal", "text.secondary", 1.0],
	"resizer": ["resizer", "text.secondary", 0],
	"port": ["port", "text.primary", 0],
	"tab": ["text_tab", "text.inactive", 0],
	"space": ["text_space", "text.inactive", 0],
	"breakpoint": ["dot", "text.primary", 0],
	"bookmark": ["pin", "text.secondary", 0],
	"executing_line": ["arrow_right", "text.primary", 0],
	"can_fold": ["chevron_down", "text.secondary", 0],
	"folded": ["chevron_right", "text.secondary", 0],
	"can_fold_code_region": ["chevron_down", "text.secondary", 0],
	"folded_code_region": ["chevron_right", "text.secondary", 0],
	"folded_eol_icon": ["ellipsis", "text.secondary", 0],
	"completion_color_bg": ["checker", "rule.divider", 0],
	"horizontal_rule": ["rule", "rule.divider", 0],
	"up": ["chevron_up", "text.secondary", 0],
	"up_hover": ["chevron_up", "text.primary", 0],
	"up_pressed": ["chevron_up", "text.primary", 0],
	"up_disabled": ["chevron_up", "text.inactive", 0],
	"down": ["chevron_down", "text.secondary", 0],
	"down_hover": ["chevron_down", "text.primary", 0],
	"down_pressed": ["chevron_down", "text.primary", 0],
	"down_disabled": ["chevron_down", "text.inactive", 0],
	"increment_pressed": ["", "", 0],
	"decrement_pressed": ["", "", 0],
	"zoom_out": ["zoom_out", "text.secondary", 0],
	"zoom_in": ["zoom_in", "text.secondary", 0],
	"zoom_reset": ["zoom_reset", "text.secondary", 0],
	"grid_toggle": ["grid", "text.secondary", 0],
	"minimap_toggle": ["minimap", "text.secondary", 0],
	"snapping_toggle": ["snap", "text.secondary", 0],
	"layout": ["layout", "text.secondary", 0],
	"menu_option": ["menu", "text.secondary", 0],
	"screen_picker": ["eyedropper", "text.secondary", 0],
	"shape_circle": ["shape_circle", "text.secondary", 0],
	"shape_rect": ["shape_rect", "text.secondary", 0],
	"shape_rect_wheel": ["shape_wheel", "text.secondary", 0],
	"add_preset": ["add", "text.secondary", 0],
	"sample_bg": ["checker", "rule.divider", 0],
	"sample_revert": ["arrow_revert", "text.secondary", 0],
	"overbright_indicator": ["dot", "text.primary", 0],
	"bar_arrow": ["chevron_down", "text.primary", 0],
	"picker_cursor": ["cursor_ring", "text.primary", 0],
	"picker_cursor_bg": ["cursor_ring_back", "surface.inset", 0],
	"color_script": ["link", "text.secondary", 0],
	"color_copy": ["copy", "text.secondary", 0],
	"bg": ["checker", "rule.divider", 0],
	"preset_bg": ["checker", "rule.divider", 0],
	"load": ["folder_open", "text.secondary", 0],
	"save": ["save", "text.secondary", 0],
	"parent_folder": ["folder_up", "text.secondary", 0],
	"back_folder": ["arrow_left", "text.secondary", 0],
	"forward_folder": ["arrow_right", "text.secondary", 0],
	"reload": ["reload", "text.secondary", 0],
	"favorite": ["pin", "text.secondary", 0],
	"toggle_hidden": ["visible", "text.secondary", 0],
	"toggle_filename_filter": ["filter", "text.secondary", 0],
	"folder": ["folder", "text.secondary", 0],
	"file": ["file", "text.secondary", 0],
	"thumbnail_mode": ["view_grid", "text.secondary", 0],
	"list_mode": ["view_list", "text.secondary", 0],
	"create_folder": ["folder_add", "text.secondary", 0],
	"sort": ["sort", "text.secondary", 0],
	"favorite_up": ["arrow_up", "text.secondary", 0],
	"favorite_down": ["arrow_down", "text.secondary", 0],
	"file_thumbnail": ["file", "text.secondary", 0],
	"folder_thumbnail": ["folder", "text.secondary", 0],
	"menu_copy_path": ["copy", "text.secondary", 1.0],
	"menu_delete": ["delete", "text.secondary", 1.0],
	"menu_new_folder": ["folder_add", "text.secondary", 1.0],
	"menu_refresh": ["reload", "text.secondary", 1.0],
	"menu_show_in_file_manager": ["folder_open", "text.secondary", 1.0],
	"menu_open_bundle": ["folder_open", "text.secondary", 1.0],
}
const ICON_BY_TYPE: Dictionary[String, Array] = {
	"CheckButton/checked": ["toggle_on", "text.primary", 1.0],
	"CheckButton/checked_disabled": ["toggle_on", "text.inactive", 1.0],
	"CheckButton/unchecked": ["toggle_off", "text.secondary", 1.0],
	"CheckButton/unchecked_disabled": ["toggle_off", "text.inactive", 1.0],
	"CheckButton/checked_mirrored": ["toggle_on|mirror", "text.primary", 1.0],
	"CheckButton/checked_disabled_mirrored": ["toggle_on|mirror", "text.inactive", 1.0],
	"CheckButton/unchecked_mirrored": ["toggle_off|mirror", "text.secondary", 1.0],
	"CheckButton/unchecked_disabled_mirrored": ["toggle_off|mirror", "text.inactive", 1.0],
	"HScrollBar/increment": ["", "", 0],
	"HScrollBar/increment_highlight": ["", "", 0],
	"HScrollBar/decrement": ["", "", 0],
	"HScrollBar/decrement_highlight": ["", "", 0],
	"VScrollBar/increment": ["", "", 0],
	"VScrollBar/increment_highlight": ["", "", 0],
	"VScrollBar/decrement": ["", "", 0],
	"VScrollBar/decrement_highlight": ["", "", 0],
	"SpinBox/updown": ["", "", 0],
	"Icons/close": ["close", "text.secondary", 0],
	"OptionButton/arrow": ["chevron_down", "text.secondary", 0],
	"ColorPicker/color_hue": ["", "", 0],
}


func _svg_source(stem: String) -> String:
	if _svg_cache.has(stem):
		return _svg_cache[stem]
	var path := ICON_DIR.path_join(stem + ".svg")
	if not FileAccess.file_exists(path):
		errors.append("missing icon " + path)
		_svg_cache[stem] = ""
		return ""
	var text := FileAccess.get_file_as_string(path).replace("\r", "").strip_edges()
	_svg_cache[stem] = text
	return text


## Width of an icon's SVG canvas in px (24 for everything except the 48 px toggles).
func _svg_width(source: String) -> float:
	var m := RegEx.create_from_string("width=\"([0-9.]+)\"").search(source)
	return float(m.get_string(1)) if m != null else 24.0


## One icon from res://ui/icons, recoloured from white to a token colour by the texture's colour
## map, so the SVG stays the only drawing and the token the only colour.
func icon(stem: String, token: String, scale: float) -> Texture2D:
	var mirror := stem.ends_with("|mirror")
	var file_stem := stem.trim_suffix("|mirror")
	var key := "%s/%s/%s/%.3f" % [stem, token, _palette_name, scale]
	if _icon_cache.has(key):
		return _icon_cache[key]
	var source := _svg_source(file_stem)
	if source.is_empty():
		return _empty_tex()
	if mirror:
		var w := _svg_width(source)
		var open_end := source.find(">") + 1
		source = source.substr(0, open_end) + "<g transform=\"translate(%d 0) scale(-1 1)\">" % int(w) + source.substr(open_end, source.rfind("</svg>") - open_end) + "</g></svg>"
	var tex := DPITexture.create_from_string(source, scale, 1.0, {Color.WHITE: colour(token)})
	tex.resource_name = file_stem
	tex.resource_scene_unique_id = _next_id("icon_" + file_stem + ("_m" if mirror else ""))
	_icon_cache[key] = tex
	return tex


func _empty_tex() -> ImageTexture:
	if _empty_texture == null:
		_empty_texture = ImageTexture.new()
		_empty_texture.resource_name = "empty"
		_empty_texture.resource_scene_unique_id = "icon_empty"
	return _empty_texture


## The hue strip of the colour picker: a functional ramp, not a UI colour.
func _hue_texture(height: int) -> Texture2D:
	var g := Gradient.new()
	g.offsets = PackedFloat32Array([0.0, 1.0 / 6.0, 2.0 / 6.0, 3.0 / 6.0, 4.0 / 6.0, 5.0 / 6.0, 1.0])
	var cols := PackedColorArray()
	for i in 7:
		cols.append(Color.from_hsv(fmod(i / 6.0, 1.0), 1.0, 1.0))
	g.colors = cols
	var t := GradientTexture2D.new()
	t.gradient = g
	t.width = 1
	t.height = height
	t.fill_from = Vector2(0, 0)
	t.fill_to = Vector2(0, 1)
	t.resource_name = "picker_hue"
	t.resource_scene_unique_id = "picker_hue"
	return t


func _default_icon(type: String, item: String, original: Texture2D) -> Texture2D:
	var key := type + "/" + item
	if key == "ColorPicker/color_hue":
		return _hue_texture(original.get_height() if original != null else 256)
	var rule: Array = ICON_BY_TYPE.get(key, ICON_BY_NAME.get(item, []))
	if rule.is_empty():
		errors.append("no icon rule for " + key)
		return _empty_tex()
	var stem: String = rule[0]
	if stem.is_empty():
		return _empty_tex()
	var scale: float = rule[2]
	if scale <= 0.0:
		var source := _svg_source(stem.trim_suffix("|mirror"))
		var w := float(original.get_width()) if original != null and original.get_width() > 0 else 16.0
		scale = snappedf(w * DEFAULT_TO_PROJECT / _svg_width(source), 0.125)
	return icon(stem, rule[1], scale)


# --- base types --------------------------------------------------------------------------------

const BOLD_FONTS: Array[String] = [
	"Window/title_font", "GraphNodeTitleLabel/font", "GraphFrameTitleLabel/font",
	"RichTextLabel/bold_font", "RichTextLabel/bold_italics_font", "PopupMenu/font_separator",
	"FoldableContainer/font", "Tree/title_button_font",
]
const MONO_FONTS: Array[String] = ["CodeEdit/font"]
const DENSE_TYPES: Array[String] = ["Tree", "ItemList", "CodeEdit"]


func _default_font(type: String, item: String) -> Font:
	var key := type + "/" + item
	if key in MONO_FONTS:
		return _fonts["mono"]
	if key in BOLD_FONTS:
		return _fonts["sans_bold"]
	return _fonts["sans"]


func _default_font_size(type: String, item: String) -> int:
	match type + "/" + item:
		"HeaderLarge/font_size":
			return _type_size("ScreenTitle")
		"HeaderMedium/font_size", "GraphFrameTitleLabel/font_size":
			return _type_size("PanelTitle")
		"HeaderSmall/font_size":
			return _type_size("Body")
	if type in DENSE_TYPES:
		return _type_size("Dense")
	return _type_size("Body")


## Extra px between lines so a block reads at 1.5 line height (L1647, L1720).
func _line_spacing(font: Font, size: int) -> int:
	var lh: float = tokens.get("line_height", 1.5)
	return maxi(0, roundi(lh * size - font.get_height(size)))


func _default_constant(type: String, item: String, original: int) -> int:
	var key := type + "/" + item
	match key:
		"Label/line_spacing", "TooltipLabel/line_spacing", "GraphNodeTitleLabel/line_spacing", "GraphFrameTitleLabel/line_spacing":
			return _line_spacing(_fonts["sans"], _type_size("Body"))
		"RichTextLabel/line_separation":
			return _line_spacing(_fonts["sans"], _type_size("Body"))
		"TextEdit/line_spacing":
			return _line_spacing(_fonts["sans"], _type_size("Body"))
		"CodeEdit/line_spacing":
			return _line_spacing(_fonts["mono"], _type_size("Dense"))
		"BoxContainer/separation", "HBoxContainer/separation", "VBoxContainer/separation", "HSeparator/separation", "VSeparator/separation":
			return _space(1)
		"GridContainer/h_separation", "GridContainer/v_separation", "FlowContainer/h_separation", "FlowContainer/v_separation", "HFlowContainer/h_separation", "HFlowContainer/v_separation", "VFlowContainer/h_separation", "VFlowContainer/v_separation":
			return _space(1)
		"Button/h_separation", "MenuButton/h_separation", "OptionButton/h_separation", "CheckBox/h_separation", "CheckButton/h_separation", "ColorPickerButton/h_separation", "MenuBar/h_separation", "TabBar/h_separation", "ItemList/h_separation", "Tree/h_separation":
			return _space(1)
		"OptionButton/arrow_margin":
			return _space(1)
		"PopupMenu/v_separation", "ItemList/v_separation", "Tree/v_separation":
			return _space(1)
		"PopupMenu/h_separation":
			return _space(1)
		"PopupMenu/item_start_padding", "PopupMenu/item_end_padding":
			return _space(2)
		"PopupMenu/indent":
			return _space(3)
		"AcceptDialog/buttons_separation":
			return _space(2)
		"TabContainer/side_margin":
			return 0
		"Window/title_height":
			return 40
		"Window/close_h_offset":
			return 28
		"Window/close_v_offset":
			return 28
		"Window/resize_margin":
			return _space(0) + 2
		"LineEdit/caret_width", "TextEdit/caret_width", "CodeEdit/caret_width":
			return _stroke("frame")
		"LineEdit/minimum_character_width":
			return 4
		"SpinBox/buttons_width":
			return 24
		"SplitContainer/separation", "HSplitContainer/separation", "VSplitContainer/separation":
			return _space(2)
		"SplitContainer/minimum_grab_thickness", "HSplitContainer/minimum_grab_thickness", "VSplitContainer/minimum_grab_thickness":
			return int(tokens.get("target_min_px", 24))
		"Tree/item_margin":
			return _space(3)
		"Tree/relationship_line_width", "Tree/parent_hl_line_width", "Tree/children_hl_line_width":
			return _stroke("rule")
		"Tree/draw_guides":
			return 1
		"GraphNode/separation":
			return _space(0)
	if item.ends_with("outline_size") or item == "outline_size":
		return 0
	if item.begins_with("shadow_"):
		return 0
	return original


func _build_defaults() -> void:
	var base := ThemeDB.get_default_theme()
	var types := base.get_type_list()
	types.sort()
	for type: String in types:
		_theme.add_type(type)
		var variation_base := base.get_type_variation_base(type)
		if variation_base != &"":
			_theme.set_type_variation(type, variation_base)
		for item: String in _sorted(base.get_color_list(type)):
			_theme.set_color(item, type, _default_colour(type, item))
		for item: String in _sorted(base.get_constant_list(type)):
			_theme.set_constant(item, type, _default_constant(type, item, base.get_constant(item, type)))
		for item: String in _sorted(base.get_font_list(type)):
			_theme.set_font(item, type, _default_font(type, item))
		for item: String in _sorted(base.get_font_size_list(type)):
			_theme.set_font_size(item, type, _default_font_size(type, item))
		for item: String in _sorted(base.get_icon_list(type)):
			_theme.set_icon(item, type, _default_icon(type, item, base.get_icon(item, type)))
		for item: String in _sorted(base.get_stylebox_list(type)):
			_theme.set_stylebox(item, type, _default_stylebox(type, item, base.get_stylebox(item, type)))


func _sorted(list: PackedStringArray) -> PackedStringArray:
	var out := list.duplicate()
	out.sort()
	return out


# --- type variations ---------------------------------------------------------------------------

func _label_variation(name: String, font: Font, size: int, colour_token: String, line_font: Font) -> void:
	_theme.add_type(name)
	_theme.set_type_variation(name, "Label")
	_theme.set_font("font", name, font)
	_theme.set_font_size("font_size", name, size)
	_theme.set_color("font_color", name, colour(colour_token))
	_theme.set_constant("line_spacing", name, _line_spacing(line_font, size))


func _build_label_variations() -> void:
	var outline := _stroke("label_outline")
	_label_variation("MenuTitle", _fonts["sans_bold"], _type_size("MenuTitle"), "text.primary", _fonts["sans_bold"])
	_label_variation("ScreenTitle", _fonts["sans_bold"], _type_size("ScreenTitle"), "text.primary", _fonts["sans_bold"])
	_label_variation("PanelTitle", _fonts["sans_bold"], _type_size("PanelTitle"), "text.primary", _fonts["sans_bold"])
	_label_variation("Body", _fonts["sans"], _type_size("Body"), "text.primary", _fonts["sans"])
	_label_variation("Dense", _fonts["sans"], _type_size("Dense"), "text.primary", _fonts["sans"])
	# Meta: body size, no tracking, the support colour; sits after or beside a heading (L1578).
	_label_variation("Meta", _fonts["sans"], _type_size("Meta"), "text.secondary", _fonts["sans"])
	# Code: the one small label; spacing_glyph 1 px and tabular figures (L1730).
	_label_variation("Code", _fonts["code"], _type_size("Code"), "text.primary", _fonts["code"])
	# PlotLabel: MSDF so zoomed plot and chart labels stay sharp, with an outline (L1733).
	_label_variation("PlotLabel", _fonts["plot"], _type_size("PlotLabel"), "text.primary", _fonts["plot"])
	_theme.set_constant("outline_size", "PlotLabel", outline)
	_theme.set_color("font_outline_color", "PlotLabel", colour("surface.inset"))
	# Mono only for expressions and message traffic (L1600).
	_label_variation(VARIATION_ALIASES["Expression"], _fonts["mono"], _type_size("Expression"), "text.primary", _fonts["mono"])
	_label_variation("Traffic", _fonts["mono"], _type_size("Traffic"), "text.primary", _fonts["mono"])


## The stripe colour of each row state. Damage stays value only (L1651); Severity pairs with
## its glyph and word (L1689).
func _stripe_colour(family: String, state: String) -> Variant:
	match family + "/" + state:
		"Stance/Friend":
			return colour("stance.friend")
		"Stance/Neutral":
			return colour("stance.neutral")
		"Stance/Suspect", "Stance/":
			return colour("stance.suspect")
		"Stance/Hostile":
			return colour("stance.hostile")
		"Severity/Pass":
			return colour("text.primary")
		"Severity/Caution":
			return colour("alert.caution")
		"Severity/Fail":
			return colour("alert.warning")
		"Severity/Advisory", "Severity/":
			return colour("text.secondary")
		"Damage/Intact", "Damage/":
			return null
		"Damage/Damaged":
			return colour(_hatch_token("damaged", "colour", "text.secondary"))
		"Damage/Heavy", "Damage/Destroyed", "Damage/Flooding":
			return colour(_hatch_token(state.to_lower(), "colour", "text.primary"))
	errors.append("no stripe for %s/%s" % [family, state])
	return null


func _hatch_token(level: String, key: String, fallback: String) -> String:
	var hatch: Dictionary = tokens.get("damage_hatch", {})
	var entry: Dictionary = hatch.get(level, {})
	return entry.get(key, fallback)


func _stripe_box(family: String, state: String) -> StyleBoxFlat:
	var c: Variant = _stripe_colour(family, state)
	var bg: Variant = null
	if family == "Damage" and state == "Destroyed":
		bg = colour(_hatch_token("destroyed", "fill", "surface.inset"))
	var sb := _flat(bg, c, 0, 0, _space(0), 2)
	sb.content_margin_left = STRIPE_PX + _space(1)
	if c != null:
		sb.border_color = c
		sb.border_width_left = STRIPE_PX
	if _hc and family == "Severity" and state == "Fail":
		# High contrast: the deep red fails 3:1, so a text.primary frame carries the edge (L1639).
		sb.border_width_left = STRIPE_PX + int(tokens.get("high_contrast_rules", {}).get("alert.warning.border_px", 2))
	return sb


func _build_stripe_variations() -> void:
	for family: String in STRIPE_STATES:
		_theme.add_type(family)
		_theme.set_type_variation(family, "Label")
		_theme.set_font("font", family, _fonts["sans"])
		_theme.set_font_size("font_size", family, _type_size(family))
		_theme.set_color("font_color", family, colour("text.primary"))
		_theme.set_constant("line_spacing", family, _line_spacing(_fonts["sans"], _type_size(family)))
		_theme.set_constant("stripe_width", family, STRIPE_PX)
		_theme.set_stylebox("normal", family, _stripe_box(family, ""))
		for state: String in STRIPE_STATES[family]:
			var sub := family + state
			_theme.add_type(sub)
			_theme.set_type_variation(sub, family)
			_theme.set_stylebox("normal", sub, _stripe_box(family, state))
			_theme.set_color("stripe_" + state.to_lower(), family, _stripe_colour(family, state) if _stripe_colour(family, state) != null else _clear("surface.base"))
	_build_badges()
	_build_damage_hatch()


## Alert and check badges (L1636-1639, L1689): fill in the alert colour, glyph and word in
## surface.inset; high contrast adds a 2 px text.primary frame to the warning fill.
func _build_badges() -> void:
	var hc_rules: Dictionary = tokens.get("high_contrast_rules", {})
	var hc_border := int(hc_rules.get("alert.warning.border_px", 2))
	var r := _radius("control")
	var caution := _flat(colour("alert.caution"), null, 0, r, _space(1), 2)
	_theme.set_stylebox("badge_caution", "Severity", caution)
	var warning := _flat(colour("alert.warning"), null, 0, r, _space(1), 2)
	if _hc:
		warning.border_color = colour(str(hc_rules.get("alert.warning.border", "text.primary")))
		warning.set_border_width_all(hc_border)
	_theme.set_stylebox("badge_warning", "Severity", warning)
	var steady := _flat(null, colour("text.primary"), 3, r, _space(1), 2)
	_theme.set_stylebox("badge_blink_off", "Severity", steady)
	_theme.set_color("badge_text", "Severity", colour("surface.inset"))
	_theme.set_color("badge_text_warning", "Severity", colour("text.primary") if _hc else colour("surface.inset"))


## Damage hatching (L1651) as theme items of the Damage type, read by the hatch widget.
func _build_damage_hatch() -> void:
	var hatch: Dictionary = tokens.get("damage_hatch", {})
	for level: String in hatch:
		var entry: Dictionary = hatch[level]
		_theme.set_color("hatch_" + level, "Damage", colour(entry.get("colour", "text.primary")))
		_theme.set_constant("hatch_%s_spacing" % level, "Damage", int(entry.get("spacing_px", 4)))
		var angles: Array = entry.get("angle_deg", [45])
		_theme.set_constant("hatch_%s_angle" % level, "Damage", int(angles[0]))
		if angles.size() > 1:
			_theme.set_constant("hatch_%s_angle_2" % level, "Damage", int(angles[1]))
		if entry.has("fill"):
			_theme.set_color("hatch_%s_fill" % level, "Damage", colour(entry["fill"]))
	_theme.set_constant("hatch_width", "Damage", _stroke("rule"))


# --- custom drawing types ----------------------------------------------------------------------

func _token_name(key: String) -> String:
	return key.replace(".", "_")


func _shared_constants(type: String) -> void:
	var strokes: Dictionary = tokens.get("stroke", {})
	for key: String in strokes:
		_theme.set_constant("stroke_" + key, type, int(strokes[key]))
	for value: Variant in tokens.get("spacing", []):
		_theme.set_constant("spacing_%d" % int(value), type, int(value))
	_theme.set_constant("target_min", type, int(tokens.get("target_min_px", 24)))
	var motion: Dictionary = tokens.get("motion_ms", {})
	for key: String in motion:
		_theme.set_constant("motion_%s_ms" % key, type, int(motion[key]))
	var blink: Dictionary = tokens.get("blink_hz", {})
	for key: String in blink:
		_theme.set_constant("blink_%s_ms" % key, type, roundi(1000.0 / float(blink[key])))


func _build_custom_types() -> void:
	for type: String in CUSTOM_TYPES:
		_theme.add_type(type)
		_shared_constants(type)
	# Plot: every palette colour by token name, plus drawing roles (L1653-1674, L1735).
	for key: String in _palette:
		_theme.set_color(_token_name(key), "Plot", colour(key))
		_theme.set_color(_token_name(key), "Gauge", colour(key))
	var hc_rules: Dictionary = tokens.get("high_contrast_rules", {})
	_theme.set_color("background", "Plot", colour("surface.inset"))
	_theme.set_color("rule_reference", "Plot", colour("rule.reference"))
	_theme.set_color("label", "Plot", colour("text.primary"))
	_theme.set_color("label_outline", "Plot", colour("surface.inset"))
	_theme.set_color("uncertainty", "Plot", colour("text.secondary"))
	_theme.set_color("history", "Plot", colour("text.secondary"))
	_theme.set_color("blocked_arc_hatch", "Plot", colour(str(hc_rules.get("blocked_arc_hatch", "text.primary"))) if _hc else colour("alert.warning"))
	_theme.set_font("label", "Plot", _fonts["plot"])
	_theme.set_font("label_bold", "Plot", _fonts["plot_bold"])
	_theme.set_font_size("label", "Plot", _type_size("PlotLabel"))
	_theme.set_constant("label_outline_size", "Plot", _stroke("label_outline"))
	_theme.set_constant("symbol_size", "Plot", _stroke("icon_grid"))
	# Chart: exact NOAA Chart 18649 entries (L1643); only ink and overlay.text carry text, and
	# ink alone in high contrast.
	for key: String in _chart:
		_theme.set_color(_token_name(key), "Chart", colour("chart." + key))
	_theme.set_color("text", "Chart", colour("chart.ink"))
	var chart_text: Array = hc_rules.get("chart_text", ["ink"])
	_theme.set_color("label_overlay", "Chart", colour("chart." + str(chart_text[0])) if _hc else colour("chart.overlay.text"))
	_theme.set_color("label_outline", "Chart", colour("chart.water.deep"))
	_theme.set_font("label", "Chart", _fonts["plot"])
	_theme.set_font("label_bold", "Chart", _fonts["plot_bold"])
	_theme.set_font_size("label", "Chart", _type_size("PlotLabel"))
	_theme.set_constant("label_outline_size", "Chart", _stroke("label_outline"))
	# Gauge: value brightest, units and scale secondary, limits quiet until crossed (L1668,
	# HUD rule 1).
	_theme.set_color("background", "Gauge", colour("surface.inset"))
	_theme.set_color("frame", "Gauge", colour("rule.divider"))
	_theme.set_color("value", "Gauge", colour("text.primary"))
	_theme.set_color("units", "Gauge", colour("text.secondary"))
	_theme.set_color("scale", "Gauge", colour("rule.reference"))
	_theme.set_color("band", "Gauge", colour("rule.divider"))
	_theme.set_color("needle", "Gauge", colour("text.primary"))
	_theme.set_color("trend", "Gauge", colour("text.secondary"))
	_theme.set_color("limit", "Gauge", colour("text.primary"))
	_theme.set_color("limit_caution_active", "Gauge", colour("alert.caution"))
	_theme.set_color("limit_warning_active", "Gauge", colour("alert.warning"))
	_theme.set_font("value", "Gauge", _fonts["sans_bold"])
	_theme.set_font("units", "Gauge", _fonts["sans"])
	_theme.set_font("scale", "Gauge", _fonts["sans"])
	_theme.set_font_size("value", "Gauge", _type_size("Body"))
	_theme.set_font_size("units", "Gauge", _type_size("Dense"))
	_theme.set_font_size("scale", "Gauge", _type_size("Dense"))
	_theme.set_constant("band_thickness", "Gauge", _space(1))
	_theme.set_constant("tick_length", "Gauge", _space(1))
	_theme.set_constant("tick_length_minor", "Gauge", _space(0))
	_theme.set_stylebox("panel", "Gauge", _flat(colour("surface.inset"), colour("rule.divider"), _stroke("rule"), _radius("frame"), _space(1), _space(0)))
