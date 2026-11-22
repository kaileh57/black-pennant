extends CanvasLayer
## Tutorial overlay: step text, pointer to the named control, Continue, demos.
## DESIGN.md L252-275. Unit: ui-tutorial.
## Reads and calls: Tutorial.step_changed, Tutorial.is_allowed, Tutorial.continue_step, Tutorial.demo_requested, Tutorial.demo_finished.
##
## Mounted by the dry dock (res://ui/editor/dry_dock.tscn), which hosts both the build and the splash basin,
## so one overlay follows the player through every step. It is hidden and inert unless `Tutorial` runs or has
## just finished. Layer 50 sits over the dock and its sortie HUD and under Screens' pushed screens, dialogs,
## toasts and the loading cover, so the pause menu and the skip question cover it.
##
## What the player sees, one thing at a time:
## - The card: the current step's instruction, "Step 3 of 11", Skip tutorial, and Continue on steps that
##   wait for it (text steps and demonstrations). On a step that asks for a splash while the previous basin is
##   still open it also offers Return to dry dock, so the player is never held in a basin.
## - The pointer: a steady 2 px ui.box frame around the control the step names (`point_at`), found through
##   the host's `control_rect(id)` (the 3D hull handles included) or by `control_id` metadata. A bare panel id
##   such as `editor.tray` frames the union of the visible controls under it. Nothing found: no frame, the
##   card still shows. The card sits beside the control on the side with room, never over it or the gauge
##   strip, and moves only when the control would end up under it.
## - Controls outside the step's allow list are disabled by the panels that own them. As a safety net this
##   overlay also swallows presses on any control whose `control_id` the step does not allow, and frames the
##   card for a second so the player looks at the instruction.
## - Demonstrations (fallback levels): `tutorial.demo.beam_handle` drags the midship beam handle narrower and
##   back inside one cancelled undo group, so the hull and the Float gauge really move and the design ends
##   exactly as it was; `tutorial.demo.fix_ghost_preview` shows the open note's Fix as a ghost. A hand glyph
##   marks the control being worked. Continue ends a demo early; a demo always reports `demo_finished` with
##   its own name, and a step change or leaving the dock stops it without a report.
## - The finish panel, on `Tutorial.finished(reward)`: the reward hull is copied into the player's blueprints
##   (`Editor.duplicate_file`, once: an unchanged copy already there is reused), then Return to main menu,
##   Open it in the dry dock, Keep sailing or Stay in the dry dock, and Copy step times (the local timing log
##   for the GitHub issue form, L266; nothing is sent).
## - Skip tutorial asks first (Screens.open_dialog), then ends any splash, stops the tutorial and goes to the
##   main menu, as the dock's Leave does.
##
## `Tutorial.stop()` emits nothing, so the overlay also polls `Tutorial.state()` four times a second.

const Kit := preload("res://ui/screens/menu_kit.gd")

## Space between the card and the control it points at, and between the card and the screen edge.
const GAP := 16.0
const EDGE := 16.0
## The frame sits this far outside the control.
const BOX_GROW := 4.0
const BOX_PX := 2.0
## With no dock view on screen (a splash basin), the resting card stays below this top band.
const REST_TOP := 96.0
## In the basin the card is placed by a search over the screen on this grid (canvas units), clear of every
## HUD cluster and the splash bar, covering as little of the own ship (and then of track symbols) as it can.
const BASIN_GRID := 32.0
## Track symbols weigh this much less than the own ship when the card has to cover something.
const BASIN_MARK_WEIGHT := 0.25
## The own ship's screen box: masts and superstructure reach this share of the hull length above the water.
const SHIP_TOP_SHARE := 0.35
const SHIP_TOP_MIN_M := 6.0
## In the basin the placement is checked again this often, so an alert that grows under the card moves it.
const BASIN_CHECK_S := 0.5
## The ship may come under the card by this much more area (canvas units squared) before it moves.
const BASIN_SLACK_PX2 := 4000.0
## With no clear spot, what covering a cluster costs against covering the own ship: the alert strip, the role
## strip, the status box, the helm, the time controls, the Bridge Board and the splash bar weigh most (the
## card must never cover them); the docked lists and the officer lines next.
const BASIN_FIRM_WEIGHT := 100.0
const BASIN_HARD_WEIGHT := 4.0
const BASIN_FIRM: Array[StringName] = [&"AlertStrip", &"RoleStrip", &"Status", &"HelmCluster", &"PacingBar", &"BridgeBoard", &"Station"]

## The instruction's measure: at most 560 (about 55 characters of Body), at least 360.
const TEXT_MAX_W := 560.0
const TEXT_MIN_W := 360.0
## The finish panel's text measure, and the room it leaves at the screen sides (margins and padding).
const FINISH_TEXT_W := 672.0
const FINISH_SIDE := 160.0
## Re-find a pointed control by metadata this often (a found one is re-checked every frame).
const RESOLVE_S := 0.5
const STATE_POLL_S := 0.25
## How long the card is framed after a press on a control the step does not allow.
const NUDGE_S := 1.0
## The demonstration's hand glyph (24 px grid, drawn at 32) on its 40 px tile, offset from the control.
const HAND_PX := 32.0
const HAND_TILE := 40.0
const HAND_OFFSET := 14.0

const DEMO_BEAM := "tutorial.demo.beam_handle"
const DEMO_FIX := "tutorial.demo.fix_ghost_preview"
## The beam step's escape is 20 s, so the demonstration ends first and Continue comes from it.
const BEAM_DEMO_S := 18.0
## Phase ends of the beam demonstration: hold, narrow, hold narrow, widen back, hold.
const BEAM_PHASES: Array[float] = [1.5, 6.5, 9.0, 15.0]
## How far the demonstration narrows the beam, as a share of the room down to the handle's minimum.
const BEAM_NARROW := 0.4
## The handle is set at most this often while it moves.
const BEAM_SET_S := 0.1
## The Fix step's escape is 12 s.
const FIX_DEMO_S := 9.0
## Text key of the Ship's Notes entry for a blocked gun arc (`NoteKind::ArcBlocked`).
const ARC_NOTE_KEY := "notes.arc_blocked"

## Bp constants, mirrored so this script parses before the extension loads.
const TUTORIAL_OFF := 0
const TUTORIAL_RUNNING := 1
const TUTORIAL_FINISHED := 2
const DOCK_SANDBOX := 0
const SESSION_IDLE := 0
const STOCK_PREFIX := "stock:"

@onready var _pointer: Control = $Pointer
@onready var _card: PanelContainer = $Card
@onready var _card_margin: MarginContainer = $Card/Margin
@onready var _card_column: VBoxContainer = $Card/Margin/Column
@onready var _after: HBoxContainer = $Card/Margin/Column/After
@onready var _after_check: TextureRect = $Card/Margin/Column/After/Check
@onready var _after_line: Label = $Card/Margin/Column/After/Line
@onready var _text: Label = $Card/Margin/Column/Text
@onready var _kits: Label = $Card/Margin/Column/Kits
@onready var _footer: HBoxContainer = $Card/Margin/Column/Footer
@onready var _progress: Label = $Card/Margin/Column/Footer/Progress
@onready var _skip: Button = $Card/Margin/Column/Footer/Skip
@onready var _back: Button = $Card/Margin/Column/Footer/Back
@onready var _continue: Button = $Card/Margin/Column/Footer/Continue
@onready var _finish: Control = $Finish
@onready var _scrim: ColorRect = $Finish/Scrim
@onready var _finish_margin: MarginContainer = $Finish/Scroll/Center/Panel/Margin
@onready var _finish_column: VBoxContainer = $Finish/Scroll/Center/Panel/Margin/Column
@onready var _finish_title: Label = $Finish/Scroll/Center/Panel/Margin/Column/Title
@onready var _finish_body: Label = $Finish/Scroll/Center/Panel/Margin/Column/Body
@onready var _actions: VBoxContainer = $Finish/Scroll/Center/Panel/Margin/Column/Actions
@onready var _actions_row: HBoxContainer = $Finish/Scroll/Center/Panel/Margin/Column/Actions/Row
@onready var _menu: Button = $Finish/Scroll/Center/Panel/Margin/Column/Actions/Menu
@onready var _open: Button = $Finish/Scroll/Center/Panel/Margin/Column/Actions/Row/Open
@onready var _stay: Button = $Finish/Scroll/Center/Panel/Margin/Column/Actions/Row/Stay
@onready var _log_note: Label = $Finish/Scroll/Center/Panel/Margin/Column/LogNote
@onready var _log_row: HBoxContainer = $Finish/Scroll/Center/Panel/Margin/Column/LogRow
@onready var _copy: Button = $Finish/Scroll/Center/Panel/Margin/Column/LogRow/Copy
@onready var _copied: Label = $Finish/Scroll/Center/Panel/Margin/Column/LogRow/Copied

var _state := TUTORIAL_OFF
## The step on screen (`TutorialStepDict`), empty when none.
var _step: Dictionary = {}
var _step_key := ""
var _target_id := ""
## Controls found by metadata for the current target, and the time left before they are looked up again.
var _target_nodes: Array[Control] = []
var _resolve_left := 0.0
var _target_rect := Rect2()
## Each framed control's rectangle (several for a kit step's rows); `_target_rect` is their union.
var _target_rects: Array[Rect2] = []
## The step whose target was last scrolled into view, so a list is scrolled once, not held.
var _scrolled_key := ""
## The step and page layout whose tight-dock page scroll was set, so the card never fights the player's own
## scrolling.
var _tight_scroll_key := ""
## The hull station whose handle a handle step points at (`Bp.NONE`: the dock's choice).
var _point_station := -1
## The card is placed again when this changes (a target appearing or going, a new step, a resize).
var _placed_key := ""
var _placing := false
var _poll_left := 0.0
var _nudge_left := 0.0
## The demonstration under way, its clock and its own state.
var _demo := ""
var _demo_t := 0.0
var _demo_set_left := 0.0
var _beam_station := -1
var _beam_from := 0.0
var _beam_low := 0.0
var _beam_group := false
var _fix_preview := false
var _hand_visible := false
var _hand_at := Vector2.ZERO
## The finish panel: where the reward is (a user:// copy, or the stock file when the copy failed).
var _reward_path := ""
var _skip_dialog: Node
## Space was pressed in the basin on a splash step: splash again once the dry dock is back.
var _splash_after_dock := false
## The dock's Splash control, found once for Splash again.
var _splash_button: BaseButton
## Time left before the basin placement is checked again (`BASIN_CHECK_S`), and how much of the own ship
## the card covered when it was placed there.
var _basin_check_left := 0.0
var _basin_ship_cover := 0.0
var _basin_last: Dictionary = {}
## How much of the hard clusters the card had to cover when it was last placed in the basin (0 when it found
## a clear spot); a card placed clear moves the frame a cluster grows under it.
var _basin_hard_cover := 0.0
## The card has the tight padding of the compact dock (`_set_card_tight`).
var _card_tight := false


func _ready() -> void:
	_pointer.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_pointer.draw.connect(_draw_pointer)
	_card.visible = false
	_finish.visible = false
	_finish.mouse_filter = Control.MOUSE_FILTER_STOP
	for l: Label in [_text, _progress, _finish_body, _after_line, _kits]:
		l.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_after_check.texture = Ui.icon(&"check_pass")
	_after_check.self_modulate = Ui.colour(&"text.primary")
	for c: Control in [_card, _card_margin, _card_column, _after, _after_check, _after_line, _text, _kits, _footer, _progress]:
		# The card is read, not clicked: only its buttons take the pointer.
		c.mouse_filter = Control.MOUSE_FILTER_IGNORE if c != _card else Control.MOUSE_FILTER_STOP
	_text.accessibility_live = AccessibilityServer.LIVE_POLITE
	_card.accessibility_name = tr(&"tutorial_overlay.card.name")
	_finish.accessibility_name = tr(&"tutorial_overlay.finish.title")
	_apply_spacing()
	_skip.pressed.connect(_on_skip)
	_back.pressed.connect(_on_back)
	_continue.pressed.connect(_on_continue)
	_menu.pressed.connect(_on_menu)
	_open.pressed.connect(_on_open)
	_stay.pressed.connect(_close_finish)
	_copy.pressed.connect(_on_copy)
	# The card's height is known only after its text wraps: place it again then.
	_card.resized.connect(_on_card_resized)
	for b: Button in [_skip, _back, _continue, _menu, _open, _stay, _copy]:
		b.focus_mode = Control.FOCUS_ALL
	_connect(&"Tutorial", &"step_changed", _on_step_changed)
	_connect(&"Tutorial", &"demo_requested", _on_demo_requested)
	_connect(&"Tutorial", &"finished", _on_finished)
	_connect(&"Tutorial", &"progress_changed", _on_progress_changed)
	_connect(&"Session", &"state_changed", _on_session_state)
	_connect(&"Ui", &"changed", _on_ui_changed)
	get_viewport().size_changed.connect(_on_resized)
	# The first step is announced before the dock (and so this overlay) exists: pull it.
	_sync()


func _exit_tree() -> void:
	_end_demo(false)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render_step()


func _connect(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _apply_spacing() -> void:
	var ui := Kit.autoload(&"Ui")
	if ui == null:
		return
	_set_card_tight(false)
	Ui.set_separation(_footer, 2)
	Ui.set_margins(_finish_margin, 4)
	Ui.set_separation(_finish_column, 3)
	Ui.set_separation(_actions, 2)
	Ui.set_separation(_actions_row, 2)
	Ui.set_separation(_log_row, 3)


## The card's padding: the normal 16 px margins and 12 px between its lines, or 8 and 8 where the compact
## dock leaves it only a short band between the gauges and the view.
func _set_card_tight(tight: bool) -> void:
	if Kit.autoload(&"Ui") == null or tight == _card_tight and _card_margin.has_theme_constant_override(&"margin_left"):
		return
	_card_tight = tight
	Ui.set_margins(_card_margin, 1 if tight else 3)
	Ui.set_separation(_card_column, 1 if tight else 2)


# --- state ---------------------------------------------------------------------------------------

func _tutorial_state() -> int:
	var s: Variant = Kit.call_on(&"Tutorial", &"state")
	return int(s) if s is int else TUTORIAL_OFF


## Pull the state and the current step (at start, and after a state change no signal reported).
func _sync() -> void:
	_state = _tutorial_state()
	if _state != TUTORIAL_RUNNING:
		_hide_card()
		return
	var step: Variant = Kit.call_on(&"Tutorial", &"step")
	if step is Dictionary and not (step as Dictionary).is_empty():
		_show_step(step)
		var demo := Kit.get_str(step, "demo")
		if not demo.is_empty():
			_start_demo(demo)
	else:
		_hide_card()


func _on_step_changed(step: Dictionary) -> void:
	_state = TUTORIAL_RUNNING
	_show_step(step)


## Part of the step was done (a lesson kit placed): re-read the step for its progress line and targets.
func _on_progress_changed() -> void:
	var step: Variant = Kit.call_on(&"Tutorial", &"step")
	if step is Dictionary and not (step as Dictionary).is_empty() and _card.visible:
		_step = step
		_render_step()
		_target_nodes.clear()
		_resolve_left = 0.0
		_placed_key = ""


func _on_demo_requested(demo_name: String) -> void:
	_start_demo(String(demo_name))


func _on_finished(reward: String) -> void:
	_state = TUTORIAL_FINISHED
	_hide_card()
	_show_finish(String(reward))


func _on_session_state(_state_code: int) -> void:
	_refresh_back()


func _on_ui_changed(what: StringName) -> void:
	if what == &"scale" or what == &"palette":
		_placed_key = ""
		_pointer.queue_redraw()
	if what == &"transparency" or what == &"palette":
		_scrim.color = Ui.scrim_colour()


func _on_resized() -> void:
	_placed_key = ""
	if _finish.visible:
		_fit_finish()


## The finish panel's text width: its full measure, narrower on a small canvas (UI scale 200%), where the
## panel scrolls instead of clipping.
func _fit_finish() -> void:
	var w := clampf(get_viewport().get_visible_rect().size.x - FINISH_SIDE, TEXT_MIN_W, FINISH_TEXT_W)
	for l: Label in [_finish_body, _log_note]:
		l.custom_minimum_size.x = w
	(_finish_margin.get_parent() as Control).custom_minimum_size.x = 0.0


func _process(delta: float) -> void:
	var dt := delta if is_finite(delta) and delta > 0.0 else 0.0
	_poll_left -= dt
	if _poll_left <= 0.0:
		_poll_left = STATE_POLL_S
		_poll()
	if _demo != "":
		_run_demo(dt)
	if _card.visible:
		_track_target(dt)
	if _nudge_left > 0.0:
		_nudge_left -= dt
		if _nudge_left <= 0.0:
			_pointer.queue_redraw()
	if _splash_after_dock and not _in_splash():
		var b := _splash_control()
		if b != null:
			_splash_after_dock = false
			b.pressed.emit()
		elif _state != TUTORIAL_RUNNING:
			_splash_after_dock = false


func _poll() -> void:
	var now := _tutorial_state()
	if now != _state:
		var was := _state
		_state = now
		if now == TUTORIAL_RUNNING:
			_sync()
		elif was == TUTORIAL_RUNNING:
			# Stopped (Leave, Skip, a new start elsewhere) or finished without our seeing the signal.
			_end_demo(false)
			_hide_card()
	if _card.visible:
		_refresh_back()


# --- the card --------------------------------------------------------------------------------------

func _show_step(step: Dictionary) -> void:
	var key := "%s#%d" % [Kit.get_str(step, "id"), Kit.get_int(step, "index")]
	if key != _step_key:
		# A demonstration belongs to the step that asked for it.
		_end_demo(false)
		_step_key = key
		_placed_key = ""
	_step = step
	_target_id = Kit.get_str(step, "point_at")
	_point_station = -1
	if _target_id == "editor.handle.beam":
		# The midship handle: widening there does the most for Float, and the demonstration works it too.
		_point_station = Kit.get_int(pick_beam(Kit.dicts(Kit.call_on(&"Editor", &"handles"))), "station", -1)
	_target_nodes.clear()
	_resolve_left = 0.0
	_target_rect = Rect2()
	_target_rects.clear()
	_render_step()
	_finish.visible = false
	_card.visible = true
	_refresh_back()
	_track_target(0.0)
	_place_card()
	_pointer.queue_redraw()
	if _continue.visible:
		Ui.focus(_continue)
	else:
		_release_focus()


func _render_step() -> void:
	if _step.is_empty():
		return
	_text.text = tr(StringName(Kit.get_str(_step, "text")))
	var count := Kit.get_int(_step, "count")
	var index := Kit.get_int(_step, "index")
	# The spec's six steps are what the player counts; several script steps make one of them.
	var lesson := Kit.get_int(_step, "lesson")
	var lessons := Kit.get_int(_step, "lessons")
	# Empty on the closing step, but kept: it is the footer's spacer that holds the buttons to the right.
	_progress.text = tr(&"tutorial_overlay.progress").format([Kit.number(lesson, &"none", 0), Kit.number(lessons, &"none", 0)]) if lesson > 0 and lessons > 0 else ""
	# What the player just did, confirmed before the next instruction.
	var after := Kit.get_str(_step, "after")
	_after.visible = not after.is_empty()
	_after_line.text = tr(StringName(after)) if not after.is_empty() else ""
	_kits.text = kits_line(_kit_names(_step.get("kits_fitted")), _kit_names(_step.get("kits_wanted")).size())
	_kits.visible = not _kits.text.is_empty()
	var last := count > 0 and index >= count - 1
	var demo := not Kit.get_str(_step, "demo").is_empty()
	_continue.visible = Kit.get_bool(_step, "needs_continue") or demo
	_continue.text = tr(&"tutorial_overlay.finish_step") if last else tr(&"tutorial_overlay.continue")
	_skip.visible = not last
	_text.accessibility_name = _text.text
	_link_focus()


## Display names of kit ids (the tray's names: `<id>.name`).
func _kit_names(ids: Variant) -> PackedStringArray:
	var out: PackedStringArray = []
	if ids is PackedStringArray or ids is Array:
		for id: Variant in ids:
			var key := StringName(String(id) + ".name")
			var text := tr(key)
			out.append(text if text != String(key) else String(id))
	return out


## The kit step's progress line: none yet, or how many of how many are on the hull and which. Empty for a
## step that asks for one kit or none.
func kits_line(fitted: PackedStringArray, wanted: int) -> String:
	var total := fitted.size() + wanted
	if total < 2:
		return ""
	if fitted.is_empty():
		return tr(&"tutorial_overlay.kits.none").format([Kit.number(total, &"none", 0)])
	return tr(&"tutorial_overlay.kits.some").format([Kit.number(fitted.size(), &"none", 0), Kit.number(total, &"none", 0), ", ".join(fitted)])


func _hide_card() -> void:
	_card.visible = false
	_step = {}
	_step_key = ""
	_target_id = ""
	_target_nodes.clear()
	_target_rect = Rect2()
	_target_rects.clear()
	_hand_visible = false
	_release_focus()
	_pointer.queue_redraw()


## The basin button moves the player to where the step can be done, so a step never waits on a place the
## player cannot reach from the card: Return to dry dock on a step that asks for a new splash while a basin
## is still open (the dock reads no keys during a splash); Splash again on a step played in the basin (steer,
## sink the patrol boat, the Aggression slider) when the player is back in the dry dock.
func _refresh_back() -> void:
	var mode := _basin_mode()
	var want := mode != &""
	if want:
		var key := &"tutorial_overlay.back_to_dock" if mode == &"leave" else &"tutorial_overlay.splash_again"
		if _back.text != tr(key):
			_back.text = tr(key)
			_placed_key = ""
	if want != _back.visible:
		_back.visible = want
		_link_focus()
		_placed_key = ""


## `&"leave"`, `&"splash"` or `&""` (no basin button) for the step on screen.
func _basin_mode() -> StringName:
	if not _card.visible or _step.is_empty():
		return &""
	if _in_splash():
		# A step played in the dry dock (a new splash, the Officers tab) while the basin is still open.
		return &"leave" if _target_id.begins_with("editor.") else &""
	var allow: Variant = _step.get("allow")
	var allows_all := true
	if allow is PackedStringArray:
		allows_all = (allow as PackedStringArray).is_empty()
	elif allow is Array:
		allows_all = (allow as Array).is_empty()
	var in_basin := allows_all and not Kit.get_bool(_step, "needs_continue") and Kit.get_str(_step, "demo").is_empty() \
		and (_target_id.is_empty() or _target_id.begins_with("hud."))
	return &"splash" if in_basin and _splash_control() != null else &""


## The host's enabled Splash control (the dock's Splash button carries `editor.launch` in tutorial mode).
func _splash_control() -> BaseButton:
	if is_instance_valid(_splash_button) and _splash_button.is_visible_in_tree() and not _splash_button.disabled:
		return _splash_button
	_splash_button = null
	var host := _host()
	if host == null:
		return null
	for c: Control in _find_targets(host, "editor.launch"):
		if c is BaseButton and c.is_visible_in_tree() and not (c as BaseButton).disabled:
			_splash_button = c as BaseButton
			break
	return _splash_button


func _in_splash() -> bool:
	return Kit.call_on(&"Editor", &"in_splash") == true


## The dry dock's splash report card is open over the dock.
func _report_open() -> bool:
	var host := _host()
	var r := host.get_node_or_null(^"ReportLayer/Report") if host != null else null
	return r is CanvasItem and (r as CanvasItem).visible


func _release_focus() -> void:
	var owner_control := get_viewport().gui_get_focus_owner()
	if owner_control != null and (_card.is_ancestor_of(owner_control) or _finish.is_ancestor_of(owner_control)):
		owner_control.release_focus()


func _link_focus() -> void:
	var row: Array[Button] = []
	for b: Button in [_skip, _back, _continue]:
		if b.visible:
			row.append(b)
	for i: int in row.size():
		var b := row[i]
		var left := row[i - 1] if i > 0 else b
		var right := row[i + 1] if i < row.size() - 1 else b
		b.focus_neighbor_left = b.get_path_to(left)
		b.focus_neighbor_right = b.get_path_to(right)
		b.focus_previous = b.get_path_to(left)
		b.focus_next = b.get_path_to(right)


func _on_continue() -> void:
	if not _demo.is_empty():
		_end_demo(true)
		return
	Kit.call_on(&"Tutorial", &"continue_step")


func _on_back() -> void:
	match _basin_mode():
		&"leave":
			Kit.call_on(&"Editor", &"splash_end")
		&"splash":
			var b := _splash_control()
			if b != null:
				b.pressed.emit()
	_refresh_back()


# --- pointing at the control ---------------------------------------------------------------------

## The node the overlay is mounted on (the dry dock), else the screen on top.
func _host() -> Node:
	var p := get_parent()
	if p != null and p.has_method(&"control_rect"):
		return p
	var screens := Kit.autoload(&"Screens")
	if screens != null and screens.has_method(&"top"):
		var top: Variant = screens.call(&"top")
		if top is Node:
			return top
	return p


func _track_target(dt: float) -> void:
	var r := _resolve_rect(dt)
	if r != _target_rect:
		_target_rect = r
		_pointer.queue_redraw()
	var screen := get_viewport().get_visible_rect()
	if _place_key(screen, _rest_area(screen)) != _placed_key or (_target_rect.has_area() and _card_rect().grow(GAP * 0.5).intersects(_target_rect)):
		_place_card()
	elif _in_basin():
		# A cluster that grew under the card (a new alert row, a track list filling as contacts come in) sends
		# the card to a free spot at once; the ship turning under it is checked twice a second, so the card
		# does not chase the picture.
		_basin_check_left -= dt
		var card := _card_rect()
		var moved := false
		if _basin_hard_cover < 1.0 and _hits(card.grow(GAP * 0.5), _basin_hard_rects()):
			_place_card()
			moved = true
		if not moved and _basin_check_left <= 0.0:
			_basin_check_left = BASIN_CHECK_S
			var ship_now := card.intersection(_own_ship_rect()).get_area()
			var hard_now := 0.0
			for h: Rect2 in _basin_hard_rects():
				hard_now += card.grow(GAP * 0.5).intersection(h).get_area()
			if ship_now > _basin_ship_cover + BASIN_SLACK_PX2 or hard_now > _basin_hard_cover + BASIN_SLACK_PX2:
				_place_card()



func _resolve_rect(dt: float) -> Rect2:
	if _target_id.is_empty():
		return Rect2()
	# The splash report covers the dock until the player closes it: frame nothing under it meanwhile.
	if _report_open() and _target_id.begins_with("editor.") and _target_id != "editor.launch":
		_target_rects.clear()
		return Rect2()
	var host := _host()
	# Hull handles are 3D gizmos only the dock can place on screen.
	if _target_id.begins_with("editor.handle.") and host != null and host.has_method(&"control_rect"):
		var station := _beam_station if _demo == DEMO_BEAM else _point_station
		var hr: Variant = host.call(&"control_rect", _target_id, station)
		if hr is Rect2 and (hr as Rect2).has_area():
			_target_rects.clear()
			return _on_screen(hr)
	_resolve_left -= dt
	var stale := false
	for c: Control in _target_nodes:
		if not is_instance_valid(c) or not c.is_visible_in_tree():
			stale = true
			break
	if stale or (_target_nodes.is_empty() and _resolve_left <= 0.0) or _resolve_left <= -RESOLVE_S * 3.0:
		_resolve_left = RESOLVE_S
		_target_nodes = _targets_for(host)
		_scroll_to_target()
	var out := Rect2()
	_target_rects.clear()
	for c: Control in _target_nodes:
		if not is_instance_valid(c) or not c.is_visible_in_tree():
			continue
		var cr := _on_screen(_clipped_rect(c))
		if not cr.has_area():
			continue
		_target_rects.append(cr)
		out = cr if not out.has_area() else out.merge(cr)
	return _on_screen(out)


## The controls a step frames: for a kit step pointing at the tray, the rows of the kits it still waits for;
## otherwise the control (or the visible controls under the panel) its `point_at` names.
func _targets_for(host: Node) -> Array[Control]:
	var wanted: Variant = _step.get("kits_wanted")
	if _target_id == "editor.tray" and wanted is PackedStringArray and not (wanted as PackedStringArray).is_empty():
		var rows: Array[Control] = []
		for k: String in wanted as PackedStringArray:
			rows.append_array(_find_targets(host, "editor.tray." + k))
		if not rows.is_empty():
			return rows
	return _find_targets(host, _target_id)


## Bring the first framed control into its scroll area once per step (a note's Fix below the fold, a slider
## further down the Officers tab, a kit row under the fold of the tray).
func _scroll_to_target() -> void:
	if _target_nodes.is_empty() or _scrolled_key == _step_key:
		return
	var c := _target_nodes[0]
	if not is_instance_valid(c) or not c.is_visible_in_tree():
		return
	_scrolled_key = _step_key
	var p := c.get_parent()
	while p != null and not (p is Viewport) and not (p is CanvasLayer):
		if p is ScrollContainer:
			(p as ScrollContainer).ensure_control_visible(c)
			return
		p = p.get_parent()


func _on_screen(r: Rect2) -> Rect2:
	if not r.has_area():
		return Rect2()
	var clipped := r.intersection(get_viewport().get_visible_rect())
	return clipped if clipped.has_area() else Rect2()


## Controls carrying `control_id` == id, or, when none does, every visible control under it (`id.` prefix).
func _find_targets(host: Node, id: String) -> Array[Control]:
	var root: Node = host if host != null else get_tree().root
	var exact: Array[Control] = []
	var under: Array[Control] = []
	_collect_targets(root, id, id + ".", exact, under)
	return exact if not exact.is_empty() else under


func _collect_targets(node: Node, id: String, prefix: String, exact: Array[Control], under: Array[Control]) -> void:
	if node == self:
		return
	if node is CanvasItem and not (node as CanvasItem).visible:
		return
	if node is Control and node.has_meta(&"control_id"):
		var cid := String(node.get_meta(&"control_id"))
		if cid == id:
			if exact.is_empty():
				exact.append(node as Control)
			return
		if cid.begins_with(prefix):
			under.append(node as Control)
			return
	for child: Node in node.get_children():
		_collect_targets(child, id, prefix, exact, under)


## A control's rectangle on screen, cut by the scroll areas and clipping panels it sits in.
func _clipped_rect(c: Control) -> Rect2:
	var r := _screen_rect(c)
	var p := c.get_parent()
	while p != null and not (p is Viewport) and not (p is CanvasLayer):
		if p is Control and (p as Control).clip_contents:
			r = r.intersection(_screen_rect(p as Control))
			if not r.has_area():
				return Rect2()
		p = p.get_parent()
	return r


func _screen_rect(c: Control) -> Rect2:
	var xf := c.get_global_transform_with_canvas()
	return Rect2(xf.origin, c.size * xf.get_scale()).abs()


func _on_card_resized() -> void:
	if _card.visible and not _placing:
		_placing = true
		_place_card()
		_placing = false


func _card_rect() -> Rect2:
	return Rect2(_card.position, _card.size)


## Beside the control on the first side with room (below, above, right, left), clear of the control, the
## gauge strip and the dock's own hint; with nothing to point at, at the top of the 3D view (over the sky,
## clear of the hull) or of the screen. A control inside the 3D view keeps that resting place if it can.
func _place_card() -> void:
	var screen := get_viewport().get_visible_rect()
	var coach := _host_rect(^"Scroll/Frame/Body/Centre") if _host_rect(^"Scroll/Frame/Body/Centre/Stage").has_area() else Rect2()
	# Lines stay under 80 characters, and the card leaves half the screen free at UI scale 200%; in the dry
	# dock it also fits the width of the 3D view it sits under.
	var measure := clampf(screen.size.x * 0.5, TEXT_MIN_W, TEXT_MAX_W)
	_text.custom_minimum_size.x = measure
	_card.custom_minimum_size.x = 0.0
	_card.reset_size()
	var size := _card.get_combined_minimum_size()
	if coach.has_area():
		# The card covers the overlay bar under the view edge to edge, as one docked bar; its lines keep the
		# reading measure.
		var room := coach.size.x - EDGE * 2.0
		if size.x > room:
			_text.custom_minimum_size.x = maxf(TEXT_MIN_W * 0.75, measure - (size.x - room))
			_card.reset_size()
		_card.custom_minimum_size.x = room
		_card.reset_size()
		size = _card.get_combined_minimum_size()
	_card.size = size
	var area := _rest_area(screen)
	_placed_key = _place_key(screen, area)
	var avoid := _avoid_rects()
	var rest := _clamp_to(screen, Vector2(area.position.x + (area.size.x - size.x) * 0.5, area.position.y + GAP), size)
	var at := rest
	if not coach.has_area() and _in_basin() and not _target_rect.has_area():
		at = _place_in_basin(screen, measure)
		_card.position = at.round()
		_pointer.queue_redraw()
		return
	if _dock_tight():
		_set_card_tight(false)
		at = _place_tight(screen, size)
		_card.position = at.round()
		_pointer.queue_redraw()
		return
	var fold := coach.has_area() and _view_below_fold(screen)
	_set_card_tight(fold)
	if fold:
		at = _place_over_fold(screen, measure)
		_card.position = at.round()
		_pointer.queue_redraw()
		return
	if coach.has_area():
		# In the dry dock the card sits at the foot of the 3D view, over the overlay bar the tutorial does not
		# use, so it never covers the hull being built, the tray, Ship's Notes or the gauges; it moves to the top
		# of the view only when the framed control is down there. The frame shows what it points at.
		var bottom := Vector2(coach.get_center().x - size.x * 0.5, coach.end.y - size.y - EDGE)
		var top := Vector2(coach.get_center().x - size.x * 0.5, area.position.y + GAP)
		var far := Rect2(screen.position - Vector2(1.0e4, 1.0e4), Vector2.ONE)
		var t := _target_rect.grow(BOX_GROW) if _target_rect.has_area() else far
		at = place_beside([screen], t, size, [bottom, top], avoid, _clamp_to(screen, bottom, size))
	elif _target_rect.has_area():
		var t := _target_rect.grow(BOX_GROW)
		var mid := t.get_center()
		var below := Vector2(mid.x - size.x * 0.5, t.end.y + GAP)
		var above := Vector2(mid.x - size.x * 0.5, t.position.y - GAP - size.y)
		var right := Vector2(t.end.x + GAP, mid.y - size.y * 0.5)
		var left := Vector2(t.position.x - GAP - size.x, mid.y - size.y * 0.5)
		# A control in the left third (the build dock) gets the card to its right, toward the view, so the
		# card does not cover the rest of the panel; one in the right third gets it below, then to its left.
		var candidates: Array[Vector2] = [below, above, right, left]
		if mid.x < screen.position.x + screen.size.x / 3.0:
			candidates = [right, below, above, left]
		elif mid.x > screen.position.x + screen.size.x * 2.0 / 3.0:
			candidates = [below, left, above, right]
		# A handle in the 3D view: the card keeps its resting place at the top of the view when that is clear
		# of the handle, so it covers sky rather than the hull, else stays in the view, off the panels beside it.
		var bounds: Array[Rect2] = [screen]
		if area != screen and area.has_point(mid):
			bounds.push_front(area)
			candidates.push_front(rest)
		at = place_beside(bounds, t, size, candidates, avoid, rest)
	_card.position = at.round()
	_pointer.queue_redraw()


## The dock is narrow and short at once (UI scale 200% on 1920 x 1080): its gauge strip shows names and
## checks only, and the 3D view, the strip and the build dock all fit on the 960 x 540 canvas with at most a
## short scroll. There is no free band for the card, so it goes where the step needs nothing.
func _dock_tight() -> bool:
	var host := _host()
	var strip := host.get_node_or_null(^"Scroll/Frame/Gauges") if host != null else null
	return strip != null and bool(strip.get(&"headline")) and _host_rect(^"Scroll/Frame/Body/Centre/Stage").has_area() and not _in_basin()


## In the tight dock: the page first scrolls (once per step) just far enough to show the whole 3D view, and no
## further than the gauge strip's top. The card then keeps clear of the view, the framed control, the gauge
## the step watches (Float on the beam step), the framed controls themselves (on the kits
## step: the rows of the kits it waits for) and the view's hint, nearest the top of the build dock.
func _place_tight(screen: Rect2, size: Vector2) -> Vector2:
	var host := _host()
	var scroll := host.get_node_or_null(^"Scroll") as ScrollContainer if host != null else null
	var stage_node := host.get_node_or_null(^"Scroll/Frame/Body/Centre/Stage") as Control if host != null else null
	var strip := host.get_node_or_null(^"Scroll/Frame/Gauges") as Control if host != null else null
	# Once per step and page layout (the view's place on the page, which the dock's tab switch for the step
	# moves a frame later), never in answer to the player's own scrolling.
	var key := "%s|%d" % [_step_key, roundi(_screen_rect(stage_node).position.y + scroll.scroll_vertical) if stage_node != null and scroll != null else -1]
	if scroll != null and stage_node != null and strip != null and _tight_scroll_key != key:
		_tight_scroll_key = key
		# In page units (screen position plus the scroll), so placing again never scrolls twice.
		var at := float(scroll.scroll_vertical)
		var bottom := _screen_rect(stage_node).end.y + at - screen.end.y
		var strip_top := _screen_rect(strip).position.y + at - screen.position.y
		scroll.scroll_vertical = ceili(clampf(bottom, 0.0, maxf(strip_top, 0.0)))
	var firm: Array[Rect2] = []
	for r: Rect2 in [_host_rect(^"Scroll/Frame/Body/Centre/Stage"), _host_rect(^"Scroll/Frame/Body/Centre/Stage/Hint")]:
		if r.has_area():
			firm.append(r)
	if _target_rect.has_area():
		firm.append(_target_rect.grow(BOX_GROW))
	for id: Variant in _step.get("watch", PackedStringArray()):
		for c: Control in _find_targets(host, String(id)):
			var r := _clipped_rect(c)
			if r.has_area():
				firm.append(r)
	var gauges := _host_rect(^"Scroll/Frame/Gauges")
	var pref := Vector2(screen.position.x + EDGE, (gauges.end.y if gauges.has_area() else screen.position.y) + GAP * 0.5)
	# The card as wide as the view's column first; when that leaves no clear spot, as wide as the screen,
	# which makes it shorter (fewer lines) so it fits the band over the top bar and the gauge strip.
	var found := tight_spot(screen, size, firm, pref)
	if found[1] > 0.5:
		var chrome := size.x - _text.custom_minimum_size.x
		var wide := screen.size.x - EDGE * 2.0
		_text.custom_minimum_size.x = maxf(TEXT_MIN_W * 0.75, wide - chrome)
		_card.custom_minimum_size.x = wide
		_card.reset_size()
		# Wrapped labels report their new height only after the next layout pass, so the shorter height is
		# measured from the fonts here.
		var wide_size := Vector2(wide, size.y - _wrapped_saving(size.x - chrome, wide - chrome))
		var second := tight_spot(screen, wide_size, firm, pref)
		if second[1] < found[1]:
			_card.size = wide_size
			return second[0]
		_text.custom_minimum_size.x = size.x - chrome
		_card.custom_minimum_size.x = size.x
		_card.reset_size()
		_card.size = size
	return found[0]


## How much shorter the card's wrapped lines get when their measure grows from `from_w` to `to_w`.
func _wrapped_saving(from_w: float, to_w: float) -> float:
	var saved := 0.0
	for l: Label in [_text, _kits, _after_line]:
		if l == null or not l.is_visible_in_tree() or l.text.is_empty():
			continue
		var font := l.get_theme_font(&"font")
		var fs := l.get_theme_font_size(&"font_size")
		if font == null:
			continue
		var a := font.get_multiline_string_size(l.text, HORIZONTAL_ALIGNMENT_LEFT, maxf(from_w, 1.0), fs).y
		var b := font.get_multiline_string_size(l.text, HORIZONTAL_ALIGNMENT_LEFT, maxf(to_w, 1.0), fs).y
		saved += maxf(a - b, 0.0)
	return saved


## Where the card goes on the tight dock: `[position, cost]`, clear of every `firm` rect (cost 0) when any
## spot is, nearest `pref`; else the spot covering least of them. Also the test entry point.
static func tight_spot(screen: Rect2, size: Vector2, firm: Array[Rect2], pref: Vector2) -> Array:
	return basin_spot(screen, size, [], Rect2(), [], pref, firm)


## The compact dock (UI scale 200%) pushes the 3D view below the fold: less than this share of the screen's
## height shows it.
const FOLD_VIEW_SHARE := 0.35


## The dock's 3D view is mostly scrolled out of sight (the compact layout at a large UI scale).
func _view_below_fold(screen: Rect2) -> bool:
	var stage := _host_rect(^"Scroll/Frame/Body/Centre/Stage")
	return stage.has_area() and stage.size.y < screen.size.y * FOLD_VIEW_SHARE


## With the view below the fold the foot of the view is off the screen, so the card spans the view's column
## (the tray and the build panels beside it stay clear) in the band between the gauges and the strip of the
## view still showing, covering neither when it fits, with tight padding to help it fit; beside the framed
## control when there is one. Where the band is too short (UI scale 200% on 1920 x 1080 leaves about 160
## canvas units for a card of about 200), the card keeps off the gauges and covers the top of that strip.
func _place_over_fold(screen: Rect2, measure: float) -> Vector2:
	# At a large UI scale the gauge cells can run past the strip's own rect: keep clear of what they draw.
	var gauges := _drawn_rect(^"Scroll/Frame/Gauges", 4).intersection(screen)
	var stage := _host_rect(^"Scroll/Frame/Body/Centre/Stage")
	var top_bar := _host_rect(^"Scroll/Frame/TopBar")
	var column := _host_rect(^"Scroll/Frame/Body/Centre")
	var bounds := Rect2(column.position.x, screen.position.y, column.size.x, screen.size.y) if column.has_area() else screen
	_text.custom_minimum_size.x = measure
	_card.custom_minimum_size.x = 0.0
	_card.reset_size()
	var chrome := _card.get_combined_minimum_size().x - measure
	_text.custom_minimum_size.x = maxf(TEXT_MIN_W * 0.75, bounds.size.x - EDGE * 2.0 - chrome)
	_card.reset_size()
	var size := _card.get_combined_minimum_size()
	_card.size = size
	# The gauges, the top bar and the framed control are firm; the strip of the view still showing gives way
	# first when the band is too short.
	var firm: Array[Rect2] = []
	for r: Rect2 in [gauges, top_bar]:
		if r.has_area():
			firm.append(r)
	if _target_rect.has_area():
		firm.append(_target_rect.grow(BOX_GROW + GAP * 0.5))
	var hard: Array[Rect2] = []
	for r: Rect2 in [stage, _host_rect(^"Scroll/Frame/Body/Centre/Stage/Hint")]:
		if r.has_area():
			hard.append(r)
	var below_gauges := Vector2(bounds.position.x + EDGE, (gauges.end.y if gauges.has_area() else screen.position.y) + GAP * 0.5)
	var found := basin_spot(bounds, size, hard, Rect2(), [], below_gauges, firm)
	return found[0]


## The union of a host control's rect and the rects of its visible descendants down to `depth` levels, each
## at least its minimum size.
func _drawn_rect(path: NodePath, depth: int) -> Rect2:
	var host := _host()
	var n := host.get_node_or_null(path) as Control if host != null else null
	if n == null or not n.is_visible_in_tree():
		return Rect2()
	var out := _screen_rect(n)
	var level: Array[Node] = [n]
	for i in depth:
		var next: Array[Node] = []
		for p: Node in level:
			for c: Node in p.get_children():
				var ctl := c as Control
				if ctl != null and ctl.visible and ctl.size.x >= 1.0 and ctl.size.y >= 1.0:
					# A cell squeezed below its minimum still draws its whole content.
					var xf := ctl.get_global_transform_with_canvas()
					out = out.merge(Rect2(xf.origin, ctl.size.max(ctl.get_combined_minimum_size()) * xf.get_scale()).abs())
					next.append(ctl)
		level = next
	return out


func _place_key(screen: Rect2, area: Rect2) -> String:
	# In the basin the card keeps its spot while the HUD's band moves; `_track_target` checks it instead.
	var where := "basin" if _in_basin() else str(area)
	return "%s|%s|%s|%s" % [_step_key, str(_target_rect.has_area()), str(screen.size), where]


## The splash basin is up: the sortie HUD shows and the dock's own view is hidden.
func _in_basin() -> bool:
	return _hud_node() != null and _hud_rect(^"Frame/Docks").has_area() and not _host_rect(^"Scroll/Frame/Body/Centre/Stage").has_area()


## The card in the basin, at the reading measure: the spot the search finds (`basin_spot`). The card is also
## handed to the HUD as an occluder, so a track symbol under it moves to its edge as it does for a docked
## panel. A label that rewraps resizes the card, and `_on_card_resized` places it again at its real size.
func _place_in_basin(screen: Rect2, _measure: float) -> Vector2:
	var hud := _hud_node()
	if hud != null and hud.has_method(&"set_host_occluders"):
		var mine: Array[Control] = [_card]
		hud.call(&"set_host_occluders", &"tutorial_card", mine)
	var hard := _basin_hard_rects(false)
	var firm := _basin_hard_rects(true)
	var ship := _own_ship_rect()
	var marks := _marker_rects()
	var weapons := _hud_rect(^"Frame/Docks/LeftDock/Column/WeaponsPanel")
	var size := _card.size
	# Preferred: low on the left, resting on the weapons panel's top edge, over the open sea between the docked
	# track list and the weapons.
	var floor_y := weapons.position.y if weapons.has_area() else screen.end.y
	var pref := Vector2(screen.position.x + EDGE, floor_y - GAP - size.y)
	var found := basin_spot(screen, size, hard, ship, marks, pref, firm)
	var best: Vector2 = found[0]
	_basin_hard_cover = 0.0
	for r: Rect2 in hard + firm:
		_basin_hard_cover += Rect2(best, size).grow(GAP * 0.5).intersection(r).get_area()
	_basin_ship_cover = Rect2(best, size).intersection(ship).get_area() if ship.has_area() else 0.0
	_basin_check_left = BASIN_CHECK_S
	_basin_last = {"card": Rect2(best, size), "ship": ship, "cost": found[1], "hard_cover": _basin_hard_cover}
	return best


## What the last basin placement saw (`card`, `ship`, `cost`), for the play-through driver's log.
func basin_report() -> Dictionary:
	return _basin_last.duplicate()


## The best spot for a card of `size` on `screen`: `[position, cost]`. A spot inside the screen's edge margin
## that touches no `hard` rect (HUD clusters, the splash bar) is required while one exists; among those the
## one covering the least of `ship` (then of `marks`, at a quarter weight) wins, ties going to the spot
## nearest `pref`. `firm` rects are kept clear like `hard` ones; with no clear spot at all, covering a `firm`
## rect costs a hundred times covering the ship and a `hard` one four times. Also the test entry point.
static func basin_spot(screen: Rect2, size: Vector2, hard: Array[Rect2], ship: Rect2, marks: Array[Rect2], pref: Vector2, firm: Array[Rect2] = []) -> Array:
	var lo := screen.position + Vector2(EDGE, EDGE)
	var hi := screen.end - size - Vector2(EDGE, EDGE)
	if hi.x < lo.x or hi.y < lo.y:
		return [_clamp_to(screen, pref, size), INF]
	var xs: Array[float] = []
	var x := lo.x
	while x < hi.x:
		xs.append(x)
		x += BASIN_GRID
	xs.append(hi.x)
	xs.append(clampf(pref.x, lo.x, hi.x))
	var ys: Array[float] = []
	var y := lo.y
	while y < hi.y:
		ys.append(y)
		y += BASIN_GRID
	ys.append(hi.y)
	ys.append(clampf(pref.y, lo.y, hi.y))
	var best := _clamp_to(screen, pref, size)
	var best_cost := INF
	var best_d := INF
	for strict: bool in [true, false]:
		for py: float in ys:
			for px: float in xs:
				var box := Rect2(Vector2(px, py), size)
				var cost := 0.0
				var blocked := false
				for tier: int in 2:
					for r: Rect2 in (firm if tier == 0 else hard):
						var hit := box.grow(GAP * 0.5).intersection(r)
						if hit.has_area():
							if strict:
								blocked = true
								break
							cost += hit.get_area() * (BASIN_FIRM_WEIGHT if tier == 0 else BASIN_HARD_WEIGHT)
					if blocked:
						break
				if blocked:
					continue
				cost += box.intersection(ship).get_area() if ship.has_area() else 0.0
				for r: Rect2 in marks:
					cost += box.intersection(r).get_area() * BASIN_MARK_WEIGHT
				var d := Vector2(px, py).distance_squared_to(pref)
				if cost < best_cost - 0.5 or (absf(cost - best_cost) <= 0.5 and d < best_d):
					best_cost = cost
					best_d = d
					best = Vector2(px, py)
		if best_cost < INF:
			break
	return [best, best_cost]


## What the card may not cover in the basin while a clear spot exists: every visible HUD cluster (the alert
## strip, the role strip, the officer lines, the helm, the docked panels) and the splash bar with its readout.
## `only_firm` true: the firm ones alone (`BASIN_FIRM` and the splash bar); false: the others alone; null: all.
func _basin_hard_rects(only_firm: Variant = null) -> Array[Rect2]:
	var out: Array[Rect2] = []
	var hud := _hud_node()
	if hud != null and hud.has_method(&"clusters"):
		for c: Control in hud.call(&"clusters"):
			if c == null or not is_instance_valid(c) or not c.is_visible_in_tree():
				continue
			if only_firm != null and (c.name in BASIN_FIRM) != bool(only_firm):
				continue
			var r := _clipped_rect(c)
			if r.size.x >= 1.0 and r.size.y >= 1.0:
				out.append(r)
	if only_firm == null or bool(only_firm):
		for path: NodePath in [^"SplashUi/Bar/Panel", ^"SplashUi/Bar/Readout"]:
			var r := _host_rect(path)
			if r.has_area():
				out.append(r)
	return out


## The own ship on screen: the box round its hull from the keel to its highest cell, projected through the
## basin's camera. The hull's pose is its grid origin (stern, port side, keel) with the bow along local +X
## and starboard along +Z (`Pose::from_heading`), so the box spans the design's length and beam from there.
## Empty without a world view or a hull, or when the ship is behind the camera.
func _own_ship_rect() -> Rect2:
	var hud := _hud_node()
	if hud == null or not hud.has_method(&"world_view"):
		return Rect2()
	var w: Variant = hud.call(&"world_view")
	if not (w is Node) or not (w as Node).has_method(&"project"):
		return Rect2()
	var m := Kit.as_dict(Kit.call_on(&"Hud", &"motion"))
	if not m.has("pos"):
		return Rect2()
	var outline := Kit.as_dict(Kit.call_on(&"Hud", &"hull_outline"))
	var length := Kit.get_float(outline, "length_m", 0.0)
	var beam := Kit.get_float(outline, "beam_m", 0.0)
	if length <= 0.0 or beam <= 0.0:
		var limits := Kit.as_dict(Kit.call_on(&"Hud", &"motion_limits"))
		length = maxf(Kit.get_float(limits, "length_m", 0.0), 10.0)
		beam = maxf(Kit.get_float(limits, "beam_m", 0.0), 3.0)
	var waterline := Kit.get_float(outline, "waterline_m", 0.0)
	var height := 0.0
	var profile: Variant = outline.get("profile")
	if profile is PackedVector2Array:
		for q: Vector2 in profile:
			height = maxf(height, q.y)
	var top := height - waterline if height > waterline else maxf(length * SHIP_TOP_SHARE, SHIP_TOP_MIN_M)
	var pos: Vector2 = m["pos"]
	var h := deg_to_rad(Kit.get_float(m, "heading_deg", 0.0))
	var fwd := Vector2(sin(h), -cos(h))
	var right := Vector2(cos(h), sin(h))
	var box := Rect2()
	var any := false
	for a: float in [0.0, 1.0]:
		for b: float in [0.0, 1.0]:
			for z: float in [-waterline, top]:
				var p: Vector2 = (w as Node).call(&"project", pos + fwd * length * a + right * beam * b, z)
				if p.x < 0.0 and p.y < 0.0:
					continue
				if not any:
					box = Rect2(p, Vector2.ZERO)
					any = true
				else:
					box = box.expand(p)
	return box.grow(GAP) if any else Rect2()


## Boxes round the track symbols on screen in the basin (the world view's markers).
func _marker_rects() -> Array[Rect2]:
	var out: Array[Rect2] = []
	var hud := _hud_node()
	var w: Variant = hud.call(&"world_view") if hud != null and hud.has_method(&"world_view") else null
	if not (w is Node) or not (w as Node).has_method(&"track_markers"):
		return out
	var m: PackedFloat32Array = (w as Node).call(&"track_markers")
	var n := m.size() / 6
	for i in n:
		if m[i * 6 + 3] < 0.5:
			continue
		out.append(Rect2(Vector2(m[i * 6 + 1], m[i * 6 + 2]) - Vector2(24.0, 24.0), Vector2(48.0, 48.0)))
	return out


## The first candidate that, kept inside the first bounds it fits, stays clear of the target and of
## `avoid`; then the first clear of the target alone; else `rest` (no side has room, as at UI scale 200% on
## a small window).
static func place_beside(bounds: Array[Rect2], target: Rect2, size: Vector2, candidates: Array[Vector2], avoid: Array[Rect2], rest: Vector2) -> Vector2:
	for strict: bool in [true, false]:
		for b: Rect2 in bounds:
			if b.size.x < size.x + EDGE * 2.0 or b.size.y < size.y + EDGE * 2.0:
				continue
			for c: Vector2 in candidates:
				var p := _clamp_to(b, c, size)
				var box := Rect2(p, size)
				if box.intersects(target.grow(GAP * 0.5)):
					continue
				if strict and _hits(box, avoid):
					continue
				return p
	return rest


static func _clamp_to(screen: Rect2, p: Vector2, size: Vector2) -> Vector2:
	var lo := screen.position + Vector2(EDGE, EDGE)
	var hi := screen.end - size - Vector2(EDGE, EDGE)
	return Vector2(clampf(p.x, lo.x, maxf(lo.x, hi.x)), clampf(p.y, lo.y, maxf(lo.y, hi.y)))


static func _hits(box: Rect2, rects: Array[Rect2]) -> bool:
	for r: Rect2 in rects:
		if box.intersects(r):
			return true
	return false


## Where the card rests when it points at nothing: the dock's 3D view while it shows, else the screen below
## its top band.
func _rest_area(screen: Rect2) -> Rect2:
	var stage := _host_rect(^"Scroll/Frame/Body/Centre/Stage")
	if stage.has_area() and stage.size.x >= _card.size.x:
		return stage
	# In the basin: the sortie HUD's open middle band, between its alert strip and its helm and weapons, so the
	# card never covers an alert, its Acknowledge or a cluster the player needs.
	var band := _basin_band()
	if band.has_area() and band.size.x >= _card.size.x:
		return band
	return Rect2(screen.position + Vector2(0.0, REST_TOP), screen.size - Vector2(0.0, REST_TOP))


## The sortie HUD's open middle band over the 3D view: below its top row (objectives, alert strip, pacing)
## and the splash bar, above its bottom row (officer lines, helm, weapons). Empty when no HUD shows.
func _basin_band() -> Rect2:
	var column := _hud_rect(^"Frame/Docks/Centre/Margin/Column")
	if not column.has_area():
		return Rect2()
	var top := column.position.y
	for r: Rect2 in [_hud_rect(^"Frame/Docks/Centre/Margin/Column/Top"), _host_rect(^"SplashUi/Bar")]:
		if r.has_area():
			top = maxf(top, r.end.y)
	var bottom := column.end.y
	var low := _hud_rect(^"Frame/Docks/Centre/Margin/Column/Bottom")
	if low.has_area():
		bottom = minf(bottom, low.position.y)
	if bottom - top < 1.0:
		return Rect2()
	return Rect2(column.position.x, top, column.size.x, bottom - top)


## The basin's sortie HUD: the dock's live one. Splash again adds the new HUD while the old one is still being
## freed, so the new one does not always carry the name `SortieHud`.
func _hud_node() -> Node:
	var host := _host()
	if host == null:
		return null
	var hud: Variant = host.get(&"_hud")
	if hud is Node and is_instance_valid(hud) and not (hud as Node).is_queued_for_deletion():
		return hud as Node
	return host.get_node_or_null(^"SortieHud")


func _hud_rect(path: NodePath) -> Rect2:
	var hud := _hud_node()
	var n := hud.get_node_or_null(path) if hud != null else null
	if n is Control and (n as Control).is_visible_in_tree():
		return _clipped_rect(n as Control)
	return Rect2()


## Places the card keeps off: the dock's gauge strip (the Float gauge is what the beam step is about), the
## view's own hint line (one instruction on screen at a time), and the sortie HUD's top and bottom rows.
func _avoid_rects() -> Array[Rect2]:
	var out: Array[Rect2] = []
	for r: Rect2 in [_host_rect(^"Scroll/Frame/Gauges"), _host_rect(^"Scroll/Frame/Body/Centre/Stage/Hint"),
			_hud_rect(^"Frame/Docks/Centre/Margin/Column/Top"), _hud_rect(^"Frame/Docks/Centre/Margin/Column/Bottom")]:
		if r.has_area():
			out.append(r)
	return out


func _host_rect(path: NodePath) -> Rect2:
	var host := _host()
	if host == null:
		return Rect2()
	var n := host.get_node_or_null(path)
	if n is Control and (n as Control).is_visible_in_tree():
		return _clipped_rect(n as Control)
	return Rect2()


func _draw_pointer() -> void:
	var box := Ui.colour(&"ui.box")
	if _card.visible and _target_rect.has_area():
		if _target_rects.size() > 1:
			for r: Rect2 in _target_rects:
				_pointer.draw_rect(r.grow(BOX_GROW), box, false, BOX_PX)
		else:
			_pointer.draw_rect(_target_rect.grow(BOX_GROW), box, false, BOX_PX)
	if _card.visible and _nudge_left > 0.0:
		_pointer.draw_rect(_card_rect().grow(BOX_GROW), box, false, BOX_PX)
	if _hand_visible:
		# The hand sits just below and right of the worked control, on an opaque tile so it reads over the
		# hull and the sea (HUD rule 18), without hiding the control itself.
		var tile := Rect2(_hand_at + Vector2(HAND_OFFSET, HAND_OFFSET), Vector2(HAND_TILE, HAND_TILE))
		_pointer.draw_rect(tile, Ui.colour(&"surface.raised"), true)
		_pointer.draw_rect(tile, Ui.colour(&"rule.divider"), false, 1.0)
		var tex := Ui.icon(&"hand")
		if tex != null:
			var inset := (HAND_TILE - HAND_PX) * 0.5
			_pointer.draw_texture_rect(tex, Rect2(tile.position + Vector2(inset, inset), Vector2(HAND_PX, HAND_PX)), false, Ui.colour(&"text.primary"))


# --- the inert net ---------------------------------------------------------------------------------

func _unhandled_input(event: InputEvent) -> void:
	# The splash key in the basin on a step that asks for a new splash: the dock reads no keys during a splash,
	# so the card does what the player asked, back to the dry dock and straight into the next basin.
	if _state == TUTORIAL_RUNNING and _card.visible and _target_id == "editor.launch" and _in_splash() \
			and event.is_action_pressed(&"splash", false, true):
		get_viewport().set_input_as_handled()
		_splash_after_dock = true
		Kit.call_on(&"Editor", &"splash_end")


func _input(event: InputEvent) -> void:
	if _finish.visible:
		if event.is_action_pressed(&"ui_cancel", false, true):
			_close_finish()
			get_viewport().set_input_as_handled()
		elif event is InputEventKey and event.is_pressed() and not _is_ui_nav(event):
			# The finish panel is modal over the dock and the basin: no dock or HUD shortcuts under it.
			get_viewport().set_input_as_handled()
		return
	if _state != TUTORIAL_RUNNING or not _card.visible:
		return
	var target: Control = null
	if event is InputEventMouseButton:
		var mb := event as InputEventMouseButton
		if mb.pressed and (mb.button_index == MOUSE_BUTTON_LEFT or mb.button_index == MOUSE_BUTTON_RIGHT):
			target = get_viewport().gui_get_hovered_control()
	elif event.is_action_pressed(&"ui_accept", false, true) or event.is_action_pressed(&"ui_select", false, true):
		target = get_viewport().gui_get_focus_owner()
	if target == null or _card.is_ancestor_of(target) or _finish.is_ancestor_of(target):
		return
	var id := _control_id_of(target)
	if id.is_empty() or _allowed(id):
		return
	get_viewport().set_input_as_handled()
	_nudge()


func _is_ui_nav(event: InputEvent) -> bool:
	for action: StringName in [&"ui_accept", &"ui_select", &"ui_cancel", &"ui_focus_next", &"ui_focus_prev", &"ui_left", &"ui_right", &"ui_up", &"ui_down"]:
		if event.is_action(action, true):
			return true
	return false


func _control_id_of(c: Node) -> String:
	var n := c
	while n != null and not (n is Viewport) and not (n is CanvasLayer):
		if n.has_meta(&"control_id"):
			return String(n.get_meta(&"control_id"))
		n = n.get_parent()
	return ""


func _allowed(id: String) -> bool:
	var ok: Variant = Kit.call_on(&"Tutorial", &"is_allowed", [id])
	return ok != false


func _nudge() -> void:
	_nudge_left = NUDGE_S
	_pointer.queue_redraw()


# --- demonstrations ------------------------------------------------------------------------------

func _start_demo(demo_name: String) -> void:
	if demo_name.is_empty() or demo_name == _demo:
		return
	_end_demo(false)
	if demo_name != Kit.get_str(_step, "demo"):
		# Not the step on screen (a late request): nothing to show it beside.
		return
	_demo = demo_name
	_demo_t = 0.0
	_demo_set_left = 0.0
	match demo_name:
		DEMO_BEAM:
			_begin_beam()
		DEMO_FIX:
			_begin_fix()
		_:
			# An animation this client does not have: end it at once so the step goes on.
			push_warning("tutorial overlay: no demonstration named %s" % demo_name)
			_end_demo.call_deferred(true)


func _demo_length() -> float:
	return BEAM_DEMO_S if _demo == DEMO_BEAM else FIX_DEMO_S


func _run_demo(dt: float) -> void:
	_demo_t += dt
	if _demo == DEMO_BEAM and _beam_group:
		_demo_set_left -= dt
		if _demo_set_left <= 0.0:
			_demo_set_left = BEAM_SET_S
			Kit.call_on(&"Editor", &"set_handle", ["beam", _beam_station, beam_value(_demo_t, _beam_from, _beam_low, Ui.reduce_motion), true])
	var show := _target_rect.has_area()
	var at := _target_rect.get_center()
	if show != _hand_visible or at != _hand_at:
		_hand_visible = show
		_hand_at = at
		_pointer.queue_redraw()
	if _demo_t >= _demo_length():
		_end_demo(true)


## The beam handle's value `t` seconds into the demonstration: hold, narrow, hold, widen back, hold. With
## reduce motion the value steps between the two widths instead of gliding.
static func beam_value(t: float, from: float, low: float, stepped: bool) -> float:
	var p := BEAM_PHASES
	if t <= p[0] or t >= p[3]:
		return from
	if stepped:
		return low if t < p[2] else from
	if t < p[1]:
		return lerpf(from, low, smoothstep(p[0], p[1], t))
	if t < p[2]:
		return low
	return lerpf(low, from, smoothstep(p[2], p[3], t))


## The midship beam handle: the station with the widest beam.
static func pick_beam(handles: Array[Dictionary]) -> Dictionary:
	var best: Dictionary = {}
	for h: Dictionary in handles:
		if Kit.get_str(h, "id") != "beam":
			continue
		if best.is_empty() or Kit.get_float(h, "value") > Kit.get_float(best, "value"):
			best = h
	return best


func _begin_beam() -> void:
	_beam_group = false
	var h := pick_beam(Kit.dicts(Kit.call_on(&"Editor", &"handles")))
	if h.is_empty():
		_beam_station = -1
		return
	_beam_station = Kit.get_int(h, "station", -1)
	_beam_from = Kit.get_float(h, "value")
	var lo := Kit.get_float(h, "min", _beam_from)
	_beam_low = _beam_from - BEAM_NARROW * maxf(0.0, _beam_from - lo)
	if _beam_station < 0 or not (_beam_low < _beam_from):
		return
	# One cancelled group: every value set below is rolled back when the demonstration ends.
	Kit.call_on(&"Editor", &"begin_group", [&"dock.undo.handle"])
	_beam_group = true
	_target_nodes.clear()


func _begin_fix() -> void:
	_fix_preview = false
	var index := pick_fix_note(Kit.dicts(Kit.call_on(&"Editor", &"notes")))
	if index >= 0:
		_fix_preview = Kit.call_on(&"Editor", &"note_preview", [index]) == true


## The note whose Fix the demonstration previews: the blocked-arc note the step is about, else the first
## note with a Fix; `Bp.NONE` when no note has one.
static func pick_fix_note(notes: Array[Dictionary]) -> int:
	var first := -1
	for n: Dictionary in notes:
		if not Kit.get_bool(n, "has_fix"):
			continue
		var text: Variant = n.get("text")
		if text is Dictionary and Kit.get_str(text, "key") == ARC_NOTE_KEY:
			return Kit.get_int(n, "index", -1)
		if first < 0:
			first = Kit.get_int(n, "index", -1)
	return first


## Stop the demonstration under way and undo what it showed; `report` tells the tutorial it ended.
func _end_demo(report: bool) -> void:
	if _demo.is_empty():
		return
	var ended := _demo
	_demo = ""
	if _beam_group:
		_beam_group = false
		Kit.call_on(&"Editor", &"end_group", [true])
	if _fix_preview:
		_fix_preview = false
		Kit.call_on(&"Editor", &"preview_clear")
	_beam_station = -1
	if _hand_visible:
		_hand_visible = false
		_pointer.queue_redraw()
	if report:
		Kit.call_on(&"Tutorial", &"demo_finished", [ended])


# --- skip ------------------------------------------------------------------------------------------

## The dock's Leave dry dock during a tutorial: the same question as Skip tutorial.
func ask_skip() -> void:
	_on_skip()


func _on_skip() -> void:
	if is_instance_valid(_skip_dialog):
		return
	var screens := Kit.autoload(&"Screens")
	if screens == null or not screens.has_method(&"open_dialog"):
		_skip_now()
		return
	var buttons: Array = [
		{"id": &"keep", "label": &"tutorial_overlay.skip.keep"},
		{"id": &"skip", "label": &"tutorial_overlay.skip.confirm"},
	]
	var d: Variant = screens.call(&"open_dialog", &"tutorial_overlay.skip.title", &"tutorial_overlay.skip.body", buttons, &"keep", &"keep")
	if d is Node and (d as Node).has_signal(&"chosen"):
		_skip_dialog = d
		(d as Node).connect(&"chosen", _on_skip_chosen)


func _on_skip_chosen(id: StringName) -> void:
	_skip_dialog = null
	if id == &"skip":
		_skip_now.call_deferred()


func _skip_now() -> void:
	_end_demo(false)
	Kit.call_on(&"Tutorial", &"stop")
	_hide_card()
	_leave_basin()
	Kit.call_on(&"Editor", &"close")
	_go(&"main_menu")


## End a splash and any session it left running.
func _leave_basin() -> void:
	if _in_splash():
		Kit.call_on(&"Editor", &"splash_end")
	var s: Variant = Kit.call_on(&"Session", &"state")
	if s is int and int(s) != SESSION_IDLE:
		Kit.call_on(&"Session", &"leave", [false])


func _go(screen: StringName, args: Dictionary = {}) -> void:
	var screens := Kit.autoload(&"Screens")
	if screens != null and screens.has_method(&"change"):
		screens.call(&"change", screen, args)


# --- the finish panel ------------------------------------------------------------------------------

func _show_finish(reward: String) -> void:
	_end_demo(false)
	_reward_path = ""
	var outcome := &"none"
	var info: Dictionary = {}
	if not reward.strip_edges().is_empty():
		var stock := stock_path(reward)
		info = Kit.as_dict(Kit.call_on(&"Editor", &"file_info", [stock]))
		var kept := _find_copy(Kit.get_str(info, "hash"))
		if not kept.is_empty():
			_reward_path = kept
			outcome = &"kept"
		else:
			var copy: Variant = Kit.call_on(&"Editor", &"duplicate_file", [stock])
			if (copy is String or copy is StringName) and not String(copy).is_empty():
				_reward_path = String(copy)
				outcome = &"added"
			else:
				_reward_path = stock
				outcome = &"failed"
	_finish_body.text = finish_text(outcome, info)
	_open.visible = not _reward_path.is_empty()
	_stay.text = tr(&"tutorial_overlay.finish.stay_basin") if _in_splash() else tr(&"tutorial_overlay.finish.stay_dock")
	_copied.visible = false
	_fit_finish()
	_scrim.color = Ui.scrim_colour()
	_finish.visible = true
	Kit.chain_vertical([_menu, _open, _stay, _copy])
	Kit.chain_horizontal([_open, _stay])
	Ui.focus(_menu)


## A reward path as the dock's stock form.
static func stock_path(path: String) -> String:
	var p := path.strip_edges()
	if p.begins_with(STOCK_PREFIX) or p.begins_with("user://") or p.begins_with("res://"):
		return p
	return STOCK_PREFIX + p


## The finish panel's body for how the reward was granted: `&"added"`, `&"kept"` (an unchanged copy was
## already there), `&"failed"` or `&"none"` (no reward).
func finish_text(outcome: StringName, info: Dictionary) -> String:
	var name_text := Kit.get_str(info, "name")
	if name_text.strip_edges().is_empty():
		name_text = tr(&"tutorial_overlay.finish.reward_name")
	var facts := tr(&"tutorial_overlay.finish.facts").format([
		Kit.number(Kit.get_float(info, "length_m"), &"m", 0),
		Kit.number(Kit.get_float(info, "displacement_t"), &"t", 0),
		Kit.number(Kit.get_int(info, "parts"), &"none", 0),
	])
	match outcome:
		&"added":
			return tr(&"tutorial_overlay.finish.added").format([name_text, facts])
		&"kept":
			return tr(&"tutorial_overlay.finish.kept").format([name_text, facts])
		&"failed":
			return tr(&"tutorial_overlay.finish.failed")
	return tr(&"tutorial_overlay.finish.none")


## A copy of the reward already in the top blueprint folder with the same content hash.
func _find_copy(hash: String) -> String:
	if hash.is_empty():
		return ""
	for e: Dictionary in Kit.dicts(Kit.call_on(&"Editor", &"browse", [""])):
		if Kit.get_bool(e, "is_folder"):
			continue
		var info: Variant = e.get("info")
		if info is Dictionary and Kit.get_str(info, "hash") == hash:
			return Kit.get_str(e, "path")
	return ""


func _close_finish() -> void:
	_finish.visible = false
	_release_focus()


func _on_menu() -> void:
	_close_finish()
	_leave_basin()
	Kit.call_on(&"Editor", &"close")
	_go(&"main_menu")


func _on_open() -> void:
	if _reward_path.is_empty():
		return
	var path := _reward_path
	_close_finish()
	_leave_basin()
	Kit.call_on(&"Editor", &"close")
	var issues: Variant = Kit.call_on(&"Editor", &"open", [DOCK_SANDBOX, {"blueprint": path}])
	var fatal := not issues is Array
	for issue: Dictionary in Kit.dicts(issues):
		fatal = fatal or Kit.get_bool(issue, "fatal")
	if fatal:
		_go(&"main_menu")
		var screens := Kit.autoload(&"Screens")
		if screens != null:
			screens.call(&"open_message", &"tutorial_overlay.open_failed.title", &"tutorial_overlay.open_failed.body")
		return
	# Arguments make the router build a fresh dock in sandbox mode instead of keeping this tutorial one.
	_go(&"dry_dock", {"blueprint": path})


func _on_copy() -> void:
	var text: Variant = Kit.call_on(&"Tutorial", &"log_text")
	if not (text is String or text is StringName):
		return
	DisplayServer.clipboard_set(String(text))
	_copied.visible = true
