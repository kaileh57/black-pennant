extends Control
## Manual aim reticle with the hand glyph and aim readout.
## DESIGN.md L949-953, L1665. Unit: ui-hud-weapons.
## Reads and calls: BpWorldView.set_reticle, BpWorldView.aim_readout, Orders.set_trigger, Orders.fire_at_reticle.
##
## Shows only in the bridge view while the player holds a weapons group or a mount (L953). It follows
## the pointer, or on a gamepad sits on the screen's centre line above the own hull (the right stick
## turns the camera; see `_pad_point`), and
## hands that canvas point to `BpWorldView.set_reticle` every frame, so the 30 Hz aim input and a
## manual shot use the ray through it. The pointer's own arrow hides while it aims over
## the open view, and comes back over any panel, menu or dialog, and the moment the role is
## released, the view changes or the HUD goes.
## Drawn, all from the Plot theme type over an outline so it reads on sky, sea and fire (rule 18):
## - a ring with a centre point; a crossed ring when the point is outside the mount's arc or reach,
##   a broken ring on open sky (nothing to lay on);
## - the hand glyph above right of the ring (rule 9: hold-to-aim shows a hand);
## - the readout beside it on an opaque backing: range and bearing of the fall of shot from the
##   mount, time of flight, the track under the reticle, and "Out of arc" with the reason.
## Firing is the HUD shell's: the left button or right trigger drives `Orders.set_trigger` while a
## weapon role is held, so this control sends no shot of its own and ignores the mouse.
## `show_aim(readout, held)` renders plain data (tests and captures).

const Kit := preload("res://ui/screens/menu_kit.gd")

const THEME_TYPE := &"Plot"
const RING_PX := 14.0
const DOT_PX := 2.0
const GLYPH_PX := 24.0
const GAP_PX := 10.0
const PAD_PX := 6.0
## Gamepad reticle start, as shares of the screen: on the centre line above the own hull (which the
## orbit camera keeps at the centre).
const PAD_AIM_START := Vector2(0.5, 0.33)
## The gamepad reticle keeps this far (canvas units) inside the aim bounds.
const PAD_AIM_INSET := 24.0
## Gamepad reticle speed at full deflection, screen heights per second.
const PAD_AIM_SPEED := 0.5
## The readout text changes at most this often (the reticle itself moves every frame).
const TEXT_INTERVAL_MS := 100
const RING_SEGMENTS := 48

var _held := false
var _live := false
var _readout: Dictionary = {}
var _lines: PackedStringArray = []
var _point := Vector2.ZERO
## Gamepad reticle position in canvas coordinates, NAN until placed (see `nudge_pad`).
var _pad_aim := Vector2(NAN, NAN)
## Where the gamepad reticle may go, in canvas coordinates: the centre view above the bottom panels,
## as the sortie HUD reports it (`set_aim_bounds`). Empty: the whole screen.
var _pad_bounds := Rect2()
var _text_ms := -TEXT_INTERVAL_MS
var _hid_cursor := false
var _world_ref: WeakRef = null
var _searched_ms := -1000
var _font: Font
var _font_size := 20
var _outline := 4


func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	focus_mode = Control.FOCUS_NONE
	# Over the HUD panels that share the centre: a gamepad reticle near the alert strip or the role
	# strip stays in sight instead of sliding under them.
	z_index = 1
	_load_theme()
	_on(&"Hud", &"board_changed", _refresh_held)
	_on(&"Hud", &"control_changed", func(_r: Dictionary, _h: int) -> void: _refresh_held())
	_on(&"Session", &"state_changed", func(_s: int) -> void: _refresh_held())
	_refresh_held()


func _exit_tree() -> void:
	_show_cursor()


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			_load_theme()
			queue_redraw()
		NOTIFICATION_VISIBILITY_CHANGED:
			_update_active()
		NOTIFICATION_TRANSLATION_CHANGED:
			_lines = readout_lines(_readout)
			queue_redraw()


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _load_theme() -> void:
	_font = get_theme_font(&"label", THEME_TYPE)
	if _font == null:
		_font = get_theme_default_font()
	_font_size = get_theme_font_size(&"label", THEME_TYPE)
	if _font_size <= 0:
		_font_size = 20
	_outline = get_theme_constant(&"label_outline_size", THEME_TYPE)


func _refresh_held() -> void:
	var state: Variant = Kit.call_on(&"Session", &"state")
	_live = state is int and int(state) == Bp.SESSION_IN_SORTIE
	var held := false
	var board := Kit.as_dict(Kit.call_on(&"Hud", &"board"))
	for r: Dictionary in Kit.dicts(board.get("held", [])):
		var k := Kit.get_int(r, "kind", -1)
		if k == Bp.ROLE_MOUNT or k == Bp.ROLE_WEAPON_GROUP:
			held = true
	if held and not _held:
		_pad_aim = Vector2(NAN, NAN)
	_held = held
	_update_active()


func _update_active() -> void:
	var active := _held and _live and is_visible_in_tree()
	set_process(active)
	if not active:
		_show_cursor()
		_readout = {}
		_lines = PackedStringArray()
	queue_redraw()


func _world() -> Node:
	if _world_ref != null:
		var w: Object = _world_ref.get_ref()
		if w != null and is_instance_valid(w) and (w as Node).is_inside_tree():
			return w as Node
		_world_ref = null
	var now := Time.get_ticks_msec()
	if now - _searched_ms < 1000:
		return null
	_searched_ms = now
	var found: Node = null
	var n: Node = get_parent()
	while n != null and found == null:
		if n.has_method(&"world_view"):
			var v: Variant = n.call(&"world_view")
			if v is Node:
				found = v
		n = n.get_parent()
	if found == null:
		var v: Variant = Kit.call_on(&"Screens", &"world_view")
		if v is Node:
			found = v
	if found != null:
		_world_ref = weakref(found)
	return found


func _process(_delta: float) -> void:
	var pad := Ui.using_gamepad and not Ui.using_mouse
	var local := _pad_point() if pad else get_local_mouse_position()
	var inside := Rect2(Vector2.ZERO, size).has_point(local)
	# Over a panel, a menu or a dialog the pointer is for that control: the arrow shows and the aim
	# holds where it was. Only over the open 3D view does the reticle take the pointer's place.
	var over_control := not pad and get_viewport().gui_get_hovered_control() != null
	if pad or not inside or over_control:
		_show_cursor()
	else:
		_hide_cursor()
	if over_control or (not pad and not inside):
		queue_redraw()
		return
	_point = local
	var w := _world()
	if w != null:
		if w.has_method(&"set_reticle"):
			w.call(&"set_reticle", get_global_transform_with_canvas() * local)
		var now := Time.get_ticks_msec()
		if now - _text_ms >= TEXT_INTERVAL_MS and w.has_method(&"aim_readout"):
			_text_ms = now
			_readout = Kit.as_dict(w.call(&"aim_readout"))
			_lines = readout_lines(_readout)
	queue_redraw()


## Where the reticle sits on a gamepad, in local coordinates. The orbit camera keeps the own hull
## at the centre of the screen, so a reticle fixed there always laid on the own ship; instead the
## right stick moves it like a pointer (`nudge_pad`), starting above the hull.
func _pad_point() -> Vector2:
	if is_nan(_pad_aim.x):
		_pad_aim = _clamp_pad(get_viewport().get_visible_rect().size * PAD_AIM_START)
	return get_global_transform_with_canvas().affine_inverse() * _pad_aim


## The open part of the centre view, in canvas coordinates; the sortie HUD sets it on layout.
func set_aim_bounds(bounds: Rect2) -> void:
	_pad_bounds = bounds
	if not is_nan(_pad_aim.x):
		_pad_aim = _clamp_pad(_pad_aim)


func _aim_bounds() -> Rect2:
	var r := _pad_bounds if _pad_bounds.has_area() else get_viewport().get_visible_rect()
	return r.grow(-PAD_AIM_INSET) if r.size.x > PAD_AIM_INSET * 4.0 and r.size.y > PAD_AIM_INSET * 4.0 else r


func _clamp_pad(p: Vector2) -> Vector2:
	var r := _aim_bounds()
	return p.clamp(r.position, r.end)


## Move the gamepad reticle by the right stick's deflection (`stick`, -1..1 each axis, down
## positive) over `delta` seconds. Returns the part of the stick that should turn the camera: the
## sideways push once the reticle is at a side edge, so training past the view keeps going.
## The sortie HUD calls it while a weapon role is held on a gamepad.
func nudge_pad(stick: Vector2, delta: float) -> Vector2:
	var view := get_viewport().get_visible_rect().size
	if view.y <= 0.0:
		return stick
	_pad_point()
	var wanted := _pad_aim + stick * PAD_AIM_SPEED * view.y * delta
	_pad_aim = _clamp_pad(wanted)
	var r := _aim_bounds()
	var turn := Vector2.ZERO
	if (wanted.x < r.position.x and stick.x < 0.0) or (wanted.x > r.end.x and stick.x > 0.0):
		turn.x = stick.x
	return turn


func _hide_cursor() -> void:
	if not _hid_cursor and Input.mouse_mode == Input.MOUSE_MODE_VISIBLE and DisplayServer.get_name() != "headless":
		Input.mouse_mode = Input.MOUSE_MODE_HIDDEN
		_hid_cursor = true


func _show_cursor() -> void:
	if _hid_cursor:
		if Input.mouse_mode == Input.MOUSE_MODE_HIDDEN:
			Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
		_hid_cursor = false


## Render a readout (`BpWorldView.aim_readout`) at `point` (local) with the role held or not. Also
## the test and capture entry point.
func show_aim(readout: Dictionary, held: bool, point: Vector2) -> void:
	_readout = readout
	_lines = readout_lines(readout)
	_held = held
	_live = true
	_point = point
	queue_redraw()


# --- text (static so tests share them) -----------------------------------------------------------

## Readout lines: "4.2 km, bearing 060", "Flight 8.4 s", "On 0002", or "Out of arc". Empty without
## a valid point.
static func readout_lines(r: Dictionary) -> PackedStringArray:
	var out: PackedStringArray = []
	if not Kit.get_bool(r, "valid"):
		if not r.is_empty():
			out.append(TranslationServer.translate(&"hud_weapons.aim.sky"))
		return out
	var range_m := maxf(Kit.get_float(r, "range_m", 0.0), 0.0)
	var rv: Variant = Kit.call_on(&"Tx", &"range", [range_m])
	var range_text: String = rv if rv is String and not (rv as String).is_empty() else Kit.number(range_m / 1000.0, &"km", 1)
	var bv: Variant = Kit.call_on(&"Tx", &"bearing", [Kit.get_float(r, "bearing_deg", 0.0)])
	var bearing_text: String = bv if bv is String and not (bv as String).is_empty() else "%03d" % (int(roundf(wrapf(Kit.get_float(r, "bearing_deg", 0.0), 0.0, 360.0))) % 360)
	out.append(TranslationServer.translate(&"hud_weapons.aim.position").format([range_text, bearing_text]))
	var tof := Kit.get_float(r, "time_of_flight_s", 0.0)
	if tof > 0.0 and is_finite(tof):
		out.append(TranslationServer.translate(&"hud_weapons.aim.flight").format([Kit.number(tof, &"s", 1)]))
	var track := Kit.get_int(r, "track", Bp.NONE)
	if track > 0:
		var tv: Variant = Kit.call_on(&"Tx", &"track_label", [track])
		out.append(TranslationServer.translate(&"hud_weapons.aim.track").format([tv if tv is String and not (tv as String).is_empty() else "%04d" % track]))
	if not Kit.get_bool(r, "in_arc", true):
		out.append(TranslationServer.translate(&"hud_weapons.aim.out_of_arc"))
	return out


# --- drawing -------------------------------------------------------------------------------------

func _draw() -> void:
	if not (_held and _live):
		return
	var c := _point
	var fg := get_theme_color(&"label", THEME_TYPE)
	var back := get_theme_color(&"label_outline", THEME_TYPE)
	var muted := get_theme_color(&"uncertainty", THEME_TYPE)
	var valid := Kit.get_bool(_readout, "valid")
	var in_arc := Kit.get_bool(_readout, "in_arc", true)
	var ring := fg if valid and in_arc else muted
	var stroke := float(get_theme_constant(&"stroke_frame", THEME_TYPE))
	if stroke <= 0.0:
		stroke = 2.0
	# Dark outline first so the marks read over bright sky and fire.
	draw_arc(c, RING_PX, 0.0, TAU, RING_SEGMENTS, back, stroke + 2.0, true)
	if valid or _readout.is_empty():
		draw_arc(c, RING_PX, 0.0, TAU, RING_SEGMENTS, ring, stroke, true)
	else:
		# Open sky: a broken ring, four arcs.
		for i in 4:
			var a0 := TAU * float(i) / 4.0 + 0.25
			draw_arc(c, RING_PX, a0, a0 + TAU / 4.0 - 0.5, 8, ring, stroke, true)
	draw_circle(c, DOT_PX + 1.0, back)
	draw_circle(c, DOT_PX, ring)
	if valid and not in_arc:
		var d := RING_PX * 0.7
		for seg: Array in [[c + Vector2(-d, -d), c + Vector2(d, d)], [c + Vector2(-d, d), c + Vector2(d, -d)]]:
			draw_line(seg[0], seg[1], back, stroke + 2.0, true)
			draw_line(seg[0], seg[1], ring, stroke, true)
	# The hand glyph: hold-to-aim (rule 9).
	var hand := Ui.icon(&"hand")
	var glyph_at := c + Vector2(RING_PX + 4.0, -RING_PX - GLYPH_PX)
	if hand != null:
		draw_rect(Rect2(glyph_at - Vector2(2, 2), Vector2(GLYPH_PX + 4.0, GLYPH_PX + 4.0)), back, true)
		draw_texture_rect(hand, Rect2(glyph_at, Vector2(GLYPH_PX, GLYPH_PX)), false, fg)
	# The readout on an opaque backing (rule 18), kept inside the view.
	if _lines.is_empty() or _font == null:
		return
	var line_h := _font.get_height(_font_size)
	var w := 0.0
	for l: String in _lines:
		w = maxf(w, _font.get_string_size(l, HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size).x)
	var box := Vector2(w + PAD_PX * 2.0, line_h * _lines.size() + PAD_PX * 2.0)
	var at := c + Vector2(RING_PX + GAP_PX, RING_PX * 0.25)
	if at.x + box.x > size.x:
		at.x = c.x - RING_PX - GAP_PX - box.x
	if at.y + box.y > size.y:
		at.y = c.y - RING_PX - box.y
	at = Vector2(maxf(at.x, 0.0), maxf(at.y, 0.0))
	var backing := back
	backing.a = 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)
	draw_rect(Rect2(at, box), backing, true)
	var y := at.y + PAD_PX + _font.get_ascent(_font_size)
	for l: String in _lines:
		var p := Vector2(at.x + PAD_PX, y)
		if _outline > 0:
			draw_string_outline(_font, p, l, HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size, _outline, back)
		draw_string(_font, p, l, HORIZONTAL_ALIGNMENT_LEFT, -1, _font_size, fg)
		y += line_h
