extends Control
## Dry dock root: layout, tools, symmetry, undo, Launch; editor input.
## DESIGN.md L118-134, L231-250. Unit: ui-editor-shell.
## Reads and calls: Editor, BpEditorView (res://world/dock_stage.tscn), Editor.launch_state, Editor.splash.
##
## Layout (L153, L233): the top bar holds the design name, undo and redo, file actions, Splash (Space) and
## Launch, the screen's largest control. The gauge strip runs under it. The left dock is where the player
## builds (Parts, Hull, Paint, Officers); its tab sets what a click in the view does, so there is no
## separate tool palette to learn. The right dock reads the design (Ship's Notes, Trials, Fit Check). The
## centre is the 3D view with its view bar (symmetry, deck slice, camera, Ship / Graph / Scripts) above
## and the overlay bar with legends below.
##
## The 3D view: res://world/dock_stage.tscn renders in a SubViewport with a world of its own, drawn by the
## Display TextureRect. The viewport renders at the screen's pixel size while its 2D size is the Display's
## size in canvas units, so `BpEditorView.pick` and the Camera3D rays take Display-local positions.
##
## Input (L235-250). The view's mouse and gestures arrive through the Display's gui_input, keys through
## `_unhandled_input`:
## - Primary click selects, Shift-click adds or removes, a drag on empty space draws a box select and a drag
##   on a part moves the selection. A part, kit or paste ghost follows the cursor; a click places it and
##   placing goes on until Esc. R and Shift+R turn, T tilts (the ghost, or one selected part).
## - Secondary click (right button, or Ctrl-click on macOS) opens the context menu. Ctrl is never a gesture
##   modifier.
## - Trackpad: two-finger drag (InputEventPanGesture with its Shift and Cmd state) and pinch
##   (InputEventMagnifyGesture) go to the view's gesture methods, which apply the settings switch, inverts
##   and speeds. Mouse: middle drag orbits, Shift + middle drag pans, the wheel zooms toward the cursor;
##   Alt + drag orbits and Alt + Shift + drag pans for a mouse without a middle button.
## - Tools: on the Hull tab a drag on a handle changes it (one undo step) and a drag on a deck or bulkhead
##   moves it; on the Paint tab a stroke paints armour or colour and a click places a marking; sculpting adds
##   or removes cells. Every drag has a numeric or click alternative in the panels.
## - Keys use the project's editor_* actions and `splash`. While a splash runs none of them is read here:
##   the sortie HUD owns R, X, F, Z, Esc and the mouse then (architect note 9).
##
## Splash (L214-216): Space or the Splash button calls `Editor.splash`. This screen then hosts the basin:
## res://world/world.tscn and the sortie HUD, the splash bar, and a Tune dock (the build tabs, moved over)
## for live edits. `Editor.splash_ended` drops the basin with `Session.leave(false)` and shows the report
## card over the dock.
##
## Tutorial (L252-275). Controls carry stable ids in `control_id` metadata: `menu.leave_dock` (always
## allowed, so a tutorial step never traps the player), `editor.undo`, `editor.redo`, `editor.save`,
## `editor.file`, `editor.name`, `editor.symmetry`, `editor.view`, `editor.camera`, `editor.splash` and
## `editor.launch`. In tutorial mode Launch is hidden and the Splash
## button carries `editor.launch`, the id the tutorial's splash step names. Clicks in the view check
## `editor.select` and `editor.tray.place`, handle drags `editor.handle.<id>`. `control_rect(id)` gives the
## canvas rectangle the tutorial overlay points at, the 3D handles included.

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")
const DesignPicker := preload("res://ui/screens/design_picker.gd")
const STAGE_SCENE := "res://world/dock_stage.tscn"
const WORLD_SCENE := "res://world/world.tscn"
const HUD_SCENE := "res://ui/hud/sortie_hud.tscn"
const TUTORIAL_OVERLAY_SCENE := "res://ui/tutorial/tutorial_overlay.tscn"

## A press moves this far (canvas units) before it counts as a drag.
const DRAG_PX := 6.0
## A handle drag across this many canvas units sweeps the handle's whole range; Shift is ten times finer.
const HANDLE_SWEEP_PX := 480.0
## Handles are drawn as rings of this radius; the view picks them within 12 canvas units.
const HANDLE_RING := 7.0
const HANDLE_RING_HOT := 10.0
const HANDLE_PICK_PX := 12.0
## Handles that belong to one station (the rest shape the whole hull); their tags name the station.
const STATION_HANDLE_IDS: Array[String] = ["beam", "draft", "deadrise", "flare", "sheer"]
## Wheel steps zoom by this factor.
const WHEEL_ZOOM := 1.12
## Cell edge in metres (L126).
const CELL_M := 0.5
## Length of an empty new hull: the first starter's 22 m (L149).
const NEW_HULL_M := 22.0
const LEFT_PARTS := 0
const LEFT_HULL := 1
const LEFT_PAINT := 2
const LEFT_OFFICERS := 3
const RIGHT_FIT := 2
## Below this canvas width the dock lays out compactly (`_apply_compact`).
const COMPACT_W := 1500.0
const LEFT_W := 520.0
const LEFT_W_COMPACT := 400.0
const RIGHT_W := 420.0
const RIGHT_W_COMPACT := 360.0
## Below this canvas height the build area (docks and view) keeps BODY_MIN_H and the page scrolls.
const SHORT_H := 760.0
const BODY_MIN_H := 500.0
## On narrow screens the 3D view never shrinks below this, however the bars around it wrap.
const STAGE_MIN_H_COMPACT := 240.0
## Launch is the screen's largest control (L233); on narrow screens it keeps its words but drops the floor.
const LAUNCH_MIN_W := 200.0

enum Drag { NONE, PRESS, ORBIT, PAN, HANDLE, PLANE, MOVE, BOX, STROKE }
enum Mode { SHIP, GRAPH, SCRIPTS }

@onready var _frame: VBoxContainer = $Scroll/Frame
@onready var _leave: Button = $Scroll/Frame/TopBar/Margin/Row/Leave
@onready var _name: LineEdit = $Scroll/Frame/TopBar/Margin/Row/Name
@onready var _save_state: Label = $Scroll/Frame/TopBar/Margin/Row/SaveState
@onready var _undo: Button = $Scroll/Frame/TopBar/Margin/Row/Undo
@onready var _redo: Button = $Scroll/Frame/TopBar/Margin/Row/Redo
@onready var _save: Button = $Scroll/Frame/TopBar/Margin/Row/Save
@onready var _file: MenuButton = $Scroll/Frame/TopBar/Margin/Row/File
@onready var _status: Label = $Scroll/Frame/TopBar/Margin/Row/Status
@onready var _splash_hint: Control = $Scroll/Frame/TopBar/Margin/Row/SplashHint
@onready var _splash: Button = $Scroll/Frame/TopBar/Margin/Row/Splash
@onready var _splash_more: Button = $Scroll/Frame/TopBar/Margin/Row/SplashMore
@onready var _launch: Button = $Scroll/Frame/TopBar/Margin/Row/Launch
@onready var _launch_content: HBoxContainer = $Scroll/Frame/TopBar/Margin/Row/Launch/Content
@onready var _launch_text: Label = $Scroll/Frame/TopBar/Margin/Row/Launch/Content/Text
@onready var _launch_caution: TextureRect = $Scroll/Frame/TopBar/Margin/Row/Launch/Content/Caution
@onready var _launch_count: Label = $Scroll/Frame/TopBar/Margin/Row/Launch/Content/Count
@onready var _body: HBoxContainer = $Scroll/Frame/Body
@onready var _left_dock: PanelContainer = $Scroll/Frame/Body/LeftDock
@onready var _left_tabs: TabContainer = $Scroll/Frame/Body/LeftDock/Tabs
@onready var _tray: Control = $Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray
@onready var _inspector: Control = $Scroll/Frame/Body/LeftDock/Tabs/Parts/Inspector
@onready var _hull: Control = $Scroll/Frame/Body/LeftDock/Tabs/Hull
@onready var _paint: Control = $Scroll/Frame/Body/LeftDock/Tabs/Paint
@onready var _right_dock: PanelContainer = $Scroll/Frame/Body/RightDock
@onready var _right_tabs: TabContainer = $Scroll/Frame/Body/RightDock/Tabs
@onready var _reports: Button = $Scroll/Frame/Body/Centre/ViewBar/Margin/Row/Reports
@onready var _symmetry: OptionButton = $Scroll/Frame/Body/Centre/ViewBar/Margin/Row/Symmetry
@onready var _radial: SpinBox = $Scroll/Frame/Body/Centre/ViewBar/Margin/Row/Radial
@onready var _slice: OptionButton = $Scroll/Frame/Body/Centre/ViewBar/Margin/Row/Slice
@onready var _camera_menu: MenuButton = $Scroll/Frame/Body/Centre/ViewBar/Margin/Row/Camera
@onready var _mode_buttons: Array[Button] = [
	$Scroll/Frame/Body/Centre/ViewBar/Margin/Row/ShipMode,
	$Scroll/Frame/Body/Centre/ViewBar/Margin/Row/GraphMode,
	$Scroll/Frame/Body/Centre/ViewBar/Margin/Row/ScriptMode,
]
@onready var _stage_box: Control = $Scroll/Frame/Body/Centre/Stage
@onready var _viewport: SubViewport = $Scroll/Frame/Body/Centre/Stage/Viewport
@onready var _display: TextureRect = $Scroll/Frame/Body/Centre/Stage/Display
@onready var _gizmos: Control = $Scroll/Frame/Body/Centre/Stage/Gizmos
@onready var _hint_panel: PanelContainer = $Scroll/Frame/Body/Centre/Stage/Hint
@onready var _hint: Label = $Scroll/Frame/Body/Centre/Stage/Hint/Margin/Text
@onready var _empty: CenterContainer = $Scroll/Frame/Body/Centre/Stage/Empty
@onready var _empty_title: Label = $Scroll/Frame/Body/Centre/Stage/Empty/Column/Title
@onready var _empty_body: Label = $Scroll/Frame/Body/Centre/Stage/Empty/Column/Body
@onready var _graph: Control = $Scroll/Frame/Body/Centre/Stage/Graph
@onready var _scripts: Control = $Scroll/Frame/Body/Centre/Stage/Scripts
@onready var _overlays: Control = $Scroll/Frame/Body/Centre/Overlays
@onready var _splash_ui: CanvasLayer = $SplashUi
@onready var _tune: Button = $SplashUi/Tune
@onready var _tune_dock: PanelContainer = $SplashUi/TuneDock
@onready var _splash_bar: Control = $SplashUi/Bar
@onready var _report: Control = $ReportLayer/Report
@onready var _splash_options: PopupPanel = $SplashOptions
@onready var _context: Control = $Context

var _stage: Node3D
var _view: Node3D
var _camera: Camera3D
var _mode: int = Mode.SHIP
var _drag: int = Drag.NONE
## The press that may become a drag: `pos`, `hit` (pick result), `shift`, `ghost`.
var _press: Dictionary = {}
var _drag_last := Vector2.ZERO
var _drag_start := Vector2.ZERO
## Handle drag: id, station, start value, range, step and the accumulated movement.
var _handle_drag: Dictionary = {}
## Plane drag: axis, index, start position, the picked point and the ray hit it started from.
var _plane_drag: Dictionary = {}
## Selection move: the plane height, the hit it started from and the cell delta applied so far.
var _move_drag: Dictionary = {}
var _box := Rect2()
## Paint, armour and sculpt strokes: cursor positions gathered since the last frame.
var _stroke_tool := -1
var _stroke_points: Array[Vector2] = []
var _stroke_seen: Dictionary[Vector3i, bool] = {}
var _stroke_total: Dictionary = {}
var _hover := Vector2(-1.0, -1.0)
var _hover_handle := ""
var _handles: Array[Dictionary] = []
var _handle_values: Dictionary[String, Dictionary] = {}
var _route_first := -1
var _opened := false
var _launch_dirty := false
var _chrome_dirty := false
var _shown := false
var _message: Variant = &""
var _in_splash := false
var _world: Node3D
var _hud: Node
var _decks := -1
var _pending_discard := Callable()
var _undo_state: Dictionary = {}
var _launch_state: Dictionary = {}
var _quiet_save := false
var _compact := false
var _after_drag := false
var _name_stale := true
var _new_path := ""


func _ready() -> void:
	_build_stage()
	_wire_controls()
	_wire_editor()
	_title_tabs()
	_apply_margins()
	_set_mode(Mode.SHIP)
	_refresh_all()
	_stage_box.resized.connect(_fit_viewport)
	_stage_box.resized.connect(_fit_hint)
	resized.connect(_apply_compact)
	_reports.toggled.connect(func(_on: bool) -> void: _apply_compact())
	_reports.tooltip_text = tr(&"editor_shell.reports_tip")
	_apply_compact()
	_fit_viewport.call_deferred()
	_mount_tutorial_overlay()


## The tutorial overlay (unit ui-tutorial) rides on the dock, which hosts both the build and the splash
## basin. It stays hidden and inert unless a tutorial runs, and points through `control_rect`.
func _mount_tutorial_overlay() -> void:
	var scene := load(TUTORIAL_OVERLAY_SCENE) as PackedScene
	if scene != null:
		add_child(scene.instantiate())


func _exit_tree() -> void:
	_end_drag(true)
	if _in_splash:
		_leave_splash_world()


## Screens calls this after `_ready`. The dock opened by `Screens.open_dry_dock` needs no arguments.
func setup(_args: Dictionary) -> void:
	_refresh_all()


## Narrow screens (a small window or UI scale up to 200%, L1716): the reports dock folds behind a toggle,
## the build dock narrows, the top bar drops its status words and Leave shows its arrow only, and the gauge
## strip wraps. The dock then scrolls up and down only, never sideways, and on a short screen the build area
## keeps a usable height and the page scrolls instead.
func _apply_compact() -> void:
	var compact := size.x > 0.0 and size.x < COMPACT_W
	_reports.visible = compact
	_right_dock.visible = not compact or _reports.button_pressed
	# On a narrow screen one side dock shows at a time: the reports take the build dock's place, so the
	# view keeps its width and nothing is pushed off the side.
	_left_dock.visible = not (compact and _reports.button_pressed)
	_stage_box.custom_minimum_size.y = STAGE_MIN_H_COMPACT if compact else 0.0
	_left_dock.custom_minimum_size.x = LEFT_W_COMPACT if compact else LEFT_W
	_right_dock.custom_minimum_size.x = RIGHT_W_COMPACT if compact else RIGHT_W
	_save_state.visible = not compact
	_status.visible = not compact
	_splash_hint.visible = not compact
	_name.custom_minimum_size.x = 160.0 if compact else 300.0
	_leave.text = "" if compact else tr(&"editor_shell.leave")
	_leave.icon = Ui.icon(&"arrow_left") if compact else null
	_leave.tooltip_text = tr(&"editor_shell.leave") if compact else ""
	_leave.accessibility_name = tr(&"editor_shell.leave")
	_body.custom_minimum_size.y = BODY_MIN_H if size.y > 0.0 and size.y < SHORT_H else 0.0
	_overlays.call(&"set_compact", compact)
	_compact = compact
	_refresh_launch()
	_refresh_hint()


func default_focus() -> Control:
	return _splash if _splash.visible and not _splash.disabled else _leave


func _apply_margins() -> void:
	for m: MarginContainer in [$Scroll/Frame/TopBar/Margin, $Scroll/Frame/Body/Centre/ViewBar/Margin, $Scroll/Frame/Body/Centre/Stage/Hint/Margin]:
		Ui.set_margins(m, 1)
	for box: BoxContainer in [$Scroll/Frame/TopBar/Margin/Row, _launch_content]:
		Ui.set_separation(box, 1)
	# The frame's bars and docks butt together; structure comes from the panels themselves.
	for box: BoxContainer in [_frame, $Scroll/Frame/Body, $Scroll/Frame/Body/Centre]:
		box.add_theme_constant_override(&"separation", 0)
	var flow := $Scroll/Frame/Body/Centre/ViewBar/Margin/Row as HFlowContainer
	flow.add_theme_constant_override(&"h_separation", Ui.spacing(1))
	flow.add_theme_constant_override(&"v_separation", Ui.spacing(1))


# --- building -------------------------------------------------------------------------------------

func _build_stage() -> void:
	var packed := load(STAGE_SCENE) as PackedScene
	if packed == null:
		push_error("dry_dock: cannot load %s" % STAGE_SCENE)
		return
	_stage = packed.instantiate() as Node3D
	_viewport.add_child(_stage)
	_view = _stage.get_node_or_null(^"View") as Node3D
	_camera = _stage.get_node_or_null(^"Camera") as Camera3D
	_display.texture = _viewport.get_texture()
	var root := get_tree().root
	_viewport.msaa_3d = root.msaa_3d
	_viewport.screen_space_aa = root.screen_space_aa
	_viewport.use_debanding = root.use_debanding
	_viewport.scaling_3d_mode = root.scaling_3d_mode
	_viewport.scaling_3d_scale = root.scaling_3d_scale
	_viewport.anisotropic_filtering_level = root.anisotropic_filtering_level
	if _view != null and _view.has_signal(&"design_shown"):
		_view.connect(&"design_shown", _on_design_shown)
	if _view != null:
		_overlays.call(&"bind_view", _view)
	_gizmos.draw.connect(_draw_gizmos)


## The SubViewport renders at the Display's size in screen pixels; its 2D size stays the canvas size.
func _fit_viewport() -> void:
	var canvas := _stage_box.size
	if canvas.x < 2.0 or canvas.y < 2.0:
		return
	var scale := get_viewport().get_final_transform().get_scale() * get_global_transform_with_canvas().get_scale()
	var px := (canvas * scale).round()
	_viewport.size = Vector2i(maxi(int(px.x), 2), maxi(int(px.y), 2))
	_viewport.size_2d_override = Vector2i(maxi(roundi(canvas.x), 2), maxi(roundi(canvas.y), 2))
	_refresh_handles()


func _wire_controls() -> void:
	_leave.pressed.connect(_on_leave)
	_name.text_submitted.connect(func(_t: String) -> void: _commit_name())
	_name.focus_exited.connect(_commit_name)
	_name.accessibility_name = tr(&"editor_shell.name.field")
	_undo.icon = Ui.icon(&"undo")
	_redo.icon = Ui.icon(&"redo")
	_undo.pressed.connect(_run_undo.bind(false))
	_redo.pressed.connect(_run_undo.bind(true))
	_save.pressed.connect(_on_save)
	_splash.pressed.connect(_do_splash)
	_splash.tooltip_text = tr(&"editor_shell.splash.tip")
	_splash_more.icon = Ui.icon(&"chevron_down")
	_splash_more.pressed.connect(_open_splash_options)
	_launch.pressed.connect(_on_launch)
	_launch_caution.texture = Ui.icon(&"check_caution")
	_launch_caution.self_modulate = Ui.colour(&"alert.caution")
	_splash_hint.set(&"action", &"splash")
	_splash_hint.set(&"compact", true)
	_file.get_popup().id_pressed.connect(_on_file_menu)
	_camera_menu.get_popup().id_pressed.connect(_on_camera_menu)
	_symmetry.item_selected.connect(_on_symmetry_picked)
	_radial.value_changed.connect(_on_radial_changed)
	_slice.item_selected.connect(_on_slice_picked)
	for i in _mode_buttons.size():
		_mode_buttons[i].pressed.connect(_set_mode.bind(i))
	_left_tabs.tab_changed.connect(_on_left_tab)
	_display.gui_input.connect(_on_view_input)
	_display.mouse_exited.connect(_on_view_exit)
	_display.set_drag_forwarding(Callable(), _can_drop_on_view, _drop_on_view)
	_display.accessibility_name = tr(&"editor_shell.view.name")
	_tune.toggled.connect(_on_tune_toggled)
	# Tune and edit rides at the start of the splash bar, so the bar's band is the only thing the basin's
	# HUD keeps below; the Tune dock opens under that band.
	_splash_bar.call(&"add_lead", _tune)
	_splash_bar.connect(&"band_changed", _on_splash_band)
	_context.connect(&"why_link", _on_why_link)
	_context.connect(&"frame_requested", _frame_view)
	_context.connect(&"closed", _refresh_hint)
	_tray.connect(&"picked", _on_tray_picked)
	_inspector.connect(&"why_requested", _open_why)
	_inspector.connect(&"route_requested", _start_route)
	_inspector.connect(&"focus_arcs", _focus_arcs)
	_hull.connect(&"tool_requested", _set_tool)
	_hull.connect(&"frame_requested", _frame_view)
	_paint.connect(&"tool_requested", _set_tool)
	_set_ids()
	_fill_menus()


func _set_ids() -> void:
	var ids: Dictionary[Control, String] = {
		_leave: "menu.leave_dock", _undo: "editor.undo", _redo: "editor.redo", _save: "editor.save",
		_file: "editor.file", _symmetry: "editor.symmetry", _radial: "editor.symmetry", _slice: "editor.view",
		_camera_menu: "editor.camera", _splash: "editor.splash", _splash_more: "editor.splash",
		_launch: "editor.launch", _name: "editor.name",
	}
	for c: Control in ids:
		c.set_meta(&"control_id", ids[c])


func _fill_menus() -> void:
	var file_menu := _file.get_popup()
	file_menu.clear(true)
	file_menu.add_item(tr(&"editor_shell.file.save_as"), 1)
	file_menu.add_item(tr(&"editor_shell.file.open"), 2)
	file_menu.add_item(tr(&"editor_shell.file.share"), 3)
	file_menu.add_item(tr(&"editor_shell.file.import"), 5)
	file_menu.add_separator()
	var starters := PopupMenu.new()
	starters.name = &"Starters"
	starters.id_pressed.connect(_on_new_design)
	starters.add_item(tr(&"editor_shell.file.new_empty"), 0)
	var list := Kit.dicts(Kit.call_on(&"Content", &"starters", [-1]))
	for i in list.size():
		var s: Dictionary = list[i]
		var label := Kit.get_str(s, "name")
		var length := Kit.get_float(s, "length_m")
		if length > 0.0:
			label = tr(&"editor_shell.file.starter").format([label, Kit.number(length, &"m", 0)])
		starters.add_item(label, i + 1)
		starters.set_item_metadata(starters.get_item_index(i + 1), Kit.get_str(s, "path"))
	file_menu.add_submenu_node_item(tr(&"editor_shell.file.new"), starters, 4)
	var cam_menu := _camera_menu.get_popup()
	cam_menu.clear()
	var frames: Array[StringName] = [&"all", &"selection", &"bow", &"stern", &"port", &"starboard", &"top", &"below"]
	for i in frames.size():
		cam_menu.add_item(tr(StringName("editor_shell.camera." + String(frames[i]))), i)
		cam_menu.set_item_metadata(i, frames[i])
	var keep := _symmetry.selected
	_symmetry.clear()
	_symmetry.add_item(tr(&"editor_shell.symmetry.none"), Bp.SYMMETRY_NONE)
	_symmetry.add_item(tr(&"editor_shell.symmetry.mirror"), Bp.SYMMETRY_MIRROR)
	_symmetry.add_item(tr(&"editor_shell.symmetry.radial"), Bp.SYMMETRY_RADIAL)
	_symmetry.select(maxi(keep, 0))
	_symmetry.tooltip_text = tr(&"editor_shell.symmetry.tip")
	_radial.tooltip_text = tr(&"editor_shell.symmetry.radial_count")
	_radial.get_line_edit().accessibility_name = tr(&"editor_shell.symmetry.radial_count")
	_splash_more.accessibility_name = tr(&"editor_shell.splash.options")
	_splash_more.tooltip_text = tr(&"editor_shell.splash.options")


func _title_tabs() -> void:
	var left: Array[StringName] = [&"editor_shell.tab.parts", &"editor_shell.tab.hull", &"editor_shell.tab.paint", &"editor_shell.tab.officers"]
	for i in mini(left.size(), _left_tabs.get_tab_count()):
		_left_tabs.set_tab_title(i, tr(left[i]))
	var right: Array[StringName] = [&"editor_shell.tab.notes", &"editor_shell.tab.trials", &"editor_shell.tab.fit"]
	for i in mini(right.size(), _right_tabs.get_tab_count()):
		_right_tabs.set_tab_title(i, tr(right[i]))
	_left_tabs.accessibility_name = tr(&"editor_shell.dock.build")
	_right_tabs.accessibility_name = tr(&"editor_shell.dock.read")


func _notification(what: int) -> void:
	if not is_node_ready():
		return
	match what:
		NOTIFICATION_TRANSLATION_CHANGED:
			_title_tabs()
			_fill_menus()
			_refresh_all()
		NOTIFICATION_APPLICATION_FOCUS_OUT, NOTIFICATION_WM_WINDOW_FOCUS_OUT:
			# A drag whose button-up never arrives must still close its undo group.
			_end_drag(false)


func _wire_editor() -> void:
	var e := _editor()
	if e == null:
		return
	e.connect(&"design_changed", _on_design_changed)
	e.connect(&"undo_changed", _mark_chrome)
	e.connect(&"selection_changed", _on_selection_changed)
	e.connect(&"tool_changed", _on_tool_changed)
	e.connect(&"ghost_changed", _on_ghost_changed)
	e.connect(&"notes_changed", _mark_launch)
	e.connect(&"saved", _on_saved)
	e.connect(&"splash_ended", _on_splash_ended)
	e.connect(&"returned_to_dock", _on_returned_to_dock)
	var session := Kit.autoload(&"Session")
	if session != null and session.has_signal(&"state_changed"):
		session.connect(&"state_changed", _on_session_state)
	var hud := Kit.autoload(&"Hud")
	if hud != null and hud.has_signal(&"why_reply"):
		hud.connect(&"why_reply", _on_why_reply)
	var tutorial := Kit.autoload(&"Tutorial")
	if tutorial != null and tutorial.has_signal(&"step_changed"):
		tutorial.connect(&"step_changed", _on_tutorial_step)


func _editor() -> Node:
	return Kit.autoload(&"Editor")


func _is_open() -> bool:
	var e := _editor()
	return e != null and bool(e.call(&"is_open"))


# --- refresh --------------------------------------------------------------------------------------

## Every state: closed (nothing to edit), opened and building the first frame, populated.
func _refresh_all() -> void:
	if not is_node_ready():
		return
	_name_stale = true
	var open := _is_open()
	_opened = open
	_empty.visible = not open or not _shown
	if not open:
		_empty_title.text = tr(&"editor_shell.empty.title")
		_empty_body.text = tr(&"editor_shell.empty.body")
	else:
		_empty_title.text = tr(&"editor_shell.loading.title")
		_empty_body.text = tr(&"editor_shell.loading.body")
	_radial.editable = open
	_display.visible = open and _mode == Mode.SHIP
	_refresh_mode_buttons()
	_refresh_chrome()
	_refresh_launch()
	_refresh_symmetry()
	_refresh_slices()
	_refresh_handles()
	_refresh_hint()
	_apply_tutorial()


func _mark_chrome() -> void:
	if not _chrome_dirty:
		_chrome_dirty = true
		_refresh_chrome.call_deferred()


func _mark_launch() -> void:
	if not _launch_dirty:
		_launch_dirty = true
		_refresh_launch.call_deferred()


func _on_design_changed(_rev: int) -> void:
	if not _opened:
		_refresh_all()
		return
	_refresh_handles()
	# While a drag edits the design every frame, only the handles follow; the rest (validation for
	# Launch, the file state, the deck list) catches up when the drag ends.
	if _drag != Drag.NONE:
		_after_drag = true
		return
	_mark_chrome()
	_mark_launch()
	_refresh_slices()


func _on_design_shown(_rev: int) -> void:
	_shown = true
	_empty.visible = false
	_refresh_handles()


func _refresh_chrome() -> void:
	_chrome_dirty = false
	var e := _editor()
	if e == null or not _is_open():
		_save_state.text = ""
		_name.text = ""
		_undo_state = {}
		_apply_tutorial()
		return
	var undo := Kit.as_dict(e.call(&"undo_state"))
	_undo_state = undo
	# `info()` hashes the whole design (milliseconds on a 90 m hull), so the name is read only when it can
	# have changed: a design opened or started, an undo or redo, a rename, a save.
	# A rename the dock did not make (the tutorial names its hull right after loading it) is read as well.
	var renamed := str(undo.get("undo_label", {})).contains("dock.undo.rename")
	if (_name_stale or renamed or Kit.get_int(undo, "depth") == 0) and not _name.has_focus():
		_name_stale = false
		_name.text = Kit.get_str(Kit.as_dict(e.call(&"info")), "name")
	var undo_text := _Text.resolve(undo.get("undo_label", {}))
	var redo_text := _Text.resolve(undo.get("redo_label", {}))
	_undo.tooltip_text = undo_text if not undo_text.is_empty() else tr(&"editor_shell.undo.none")
	_redo.tooltip_text = redo_text if not redo_text.is_empty() else tr(&"editor_shell.redo.none")
	_undo.accessibility_name = _undo.tooltip_text
	_redo.accessibility_name = _redo.tooltip_text
	var dirty := Kit.get_bool(undo, "dirty")
	var path := String(e.call(&"design_path"))
	var on_disk := path.begins_with("user://")
	if dirty:
		_save_state.text = tr(&"editor_shell.state.unsaved")
	elif not on_disk:
		_save_state.text = tr(&"editor_shell.state.stock")
	else:
		_save_state.text = tr(&"editor_shell.state.saved")
	_save.tooltip_text = tr(&"editor_shell.save.tip")
	_apply_tutorial()


## Launch carries the analysis unit's answer: blocked only by validation, with a Caution triangle and the
## open-note count while notes are open (L188). Coalesced to once a frame, because validation is not free.
func _refresh_launch() -> void:
	_launch_dirty = false
	var e := _editor()
	var state := Kit.as_dict(e.call(&"launch_state")) if e != null and _is_open() else {}
	_launch_state = state
	var tutorial := _dock_mode() == Bp.DOCK_TUTORIAL
	_launch.visible = not tutorial
	_splash.set_meta(&"control_id", "editor.launch" if tutorial else "editor.splash")
	_splash_more.set_meta(&"control_id", "editor.launch" if tutorial else "editor.splash")
	_launch_text.text = _Text.resolve(state.get("text", &"dock.launch.launch"))
	if _launch_text.text.is_empty():
		_launch_text.text = tr(&"dock.launch.launch")
	var notes := Kit.get_int(state, "open_notes")
	var caution := Kit.get_bool(state, "caution") and notes > 0
	_launch_caution.visible = caution
	_launch_count.visible = caution
	_launch_count.text = Kit.number(float(notes), &"none", 0)
	if caution:
		_launch.accessibility_name = tr(&"editor_shell.launch.with_notes").format([_launch_text.text, _launch_count.text])
		_launch.tooltip_text = tr(&"editor_shell.launch.notes_tip")
	else:
		_launch.accessibility_name = _launch_text.text
		_launch.tooltip_text = tr(&"editor_shell.launch.tip")
	var w := _launch_content.get_combined_minimum_size().x + float(Ui.spacing(3)) * 2.0
	_launch.custom_minimum_size.x = w if _compact else maxf(LAUNCH_MIN_W, w)
	if _right_tabs.get_tab_count() > RIGHT_FIT:
		_right_tabs.set_tab_hidden(RIGHT_FIT, _dock_mode() != Bp.DOCK_CAMPAIGN)
	_apply_tutorial()


func _dock_mode() -> int:
	var e := _editor()
	return int(e.call(&"dock_mode")) if e != null else -1


func _refresh_symmetry() -> void:
	var e := _editor()
	if e == null:
		return
	var s := Kit.as_dict(e.call(&"symmetry"))
	var kind := Kit.get_int(s, "symmetry", Bp.SYMMETRY_MIRROR)
	var index := _symmetry.get_item_index(kind)
	if index >= 0 and _symmetry.selected != index:
		_symmetry.select(index)
	_radial.visible = kind == Bp.SYMMETRY_RADIAL and _mode == Mode.SHIP
	_radial.set_value_no_signal(float(Kit.get_int(s, "radial_count", 2)))


func _refresh_slices() -> void:
	var e := _editor()
	var hull := Kit.as_dict(e.call(&"hull")) if e != null else {}
	var decks: Variant = hull.get("decks", PackedFloat32Array())
	var n: int = (decks as PackedFloat32Array).size() if decks is PackedFloat32Array else 0
	if n == _decks:
		return
	var keep := maxi(_slice.selected, 0)
	_decks = n
	_slice.clear()
	_slice.add_item(tr(&"editor_shell.slice.whole"), 0)
	for i in n:
		_slice.add_item(tr(&"editor_shell.slice.deck").format([Kit.number(float(i + 1), &"none", 0)]), i + 1)
	_slice.select(clampi(keep, 0, n))
	_slice.tooltip_text = tr(&"editor_shell.slice.tip")
	if keep > n and _view != null:
		_view.call(&"set_deck_slice", -1)


func _refresh_mode_buttons() -> void:
	var e := _editor()
	var limits := Kit.as_dict(e.call(&"limits")) if e != null else {}
	var max_layer := Kit.get_int(limits, "max_layer", Bp.LAYER_GRAPH)
	_mode_buttons[Mode.GRAPH].visible = max_layer >= Bp.LAYER_GRAPH
	# A build without Luau has nothing behind Scripts but a notice, so the mode is not offered (L374).
	var app := Kit.autoload(&"App")
	var luau := app != null and Kit.get_bool(Kit.as_dict(app.call(&"build_info")), "luau")
	_mode_buttons[Mode.SCRIPTS].visible = max_layer >= Bp.LAYER_SCRIPT and luau
	_mode_buttons[Mode.GRAPH].tooltip_text = tr(&"editor_shell.mode.graph_tip")
	_mode_buttons[Mode.SCRIPTS].tooltip_text = tr(&"editor_shell.mode.scripts_tip")
	if not _mode_buttons[_mode].visible:
		_set_mode(Mode.SHIP)


func _set_mode(mode: int) -> void:
	_mode = mode
	for i in _mode_buttons.size():
		_mode_buttons[i].set_pressed_no_signal(i == mode)
	var ship := mode == Mode.SHIP
	_display.visible = ship and _is_open()
	_gizmos.visible = ship
	_hint_panel.visible = ship
	_overlays.visible = ship
	_graph.visible = mode == Mode.GRAPH
	_scripts.visible = mode == Mode.SCRIPTS
	# Symmetry, the deck slice and the camera act on the 3D view only.
	for c: Control in [_symmetry, _slice, _camera_menu]:
		c.visible = ship
	_radial.visible = ship and _symmetry.get_selected_id() == Bp.SYMMETRY_RADIAL
	_viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS if ship and not _in_splash else SubViewport.UPDATE_DISABLED
	if not ship:
		_end_drag(false)
	_refresh_hint()


# --- the hint line --------------------------------------------------------------------------------

## One line in the top right of the view, over the sky the default camera leaves there: what a click does now,
## or a refusal.
func _refresh_hint() -> void:
	if not is_node_ready():
		return
	_refresh_hint_inner()
	_fit_hint.call_deferred()


## The hint hugs its text, up to the view's width less a margin each side, and wraps beyond that.
func _fit_hint() -> void:
	if not _hint_panel.visible:
		return
	var font := _hint.get_theme_font(&"font")
	var size := _hint.get_theme_font_size(&"font_size")
	var pad := float(Ui.spacing(1)) * 2.0
	var room := maxf(_stage_box.size.x - 24.0 - pad, 120.0)
	var want := font.get_string_size(_hint.text, HORIZONTAL_ALIGNMENT_LEFT, -1, size).x + 2.0
	_hint.custom_minimum_size.x = minf(want, room)
	var fit := _hint_panel.get_combined_minimum_size()
	_hint_panel.offset_left = _hint_panel.offset_right - fit.x
	# The hint sits at the foot of the view, over the water, so it never hides the masts or a ghost.
	_hint_panel.offset_top = _hint_panel.offset_bottom - fit.y


func _refresh_hint_inner() -> void:
	var ship := _mode == Mode.SHIP
	if not _Text.is_empty(_message):
		_hint.text = _Text.resolve(_message)
		_hint_panel.visible = ship
		return
	if _compact:
		# Narrow screens keep the view clear of the standing hint; refusals still show above.
		_hint_panel.visible = false
		return
	var e := _editor()
	if e == null or not _is_open():
		_hint_panel.visible = false
		return
	_hint_panel.visible = ship
	# While a tutorial runs, its card (ui-tutorial) is the one instruction on screen; refusals still show.
	var tutorial: bool = Kit.call_on(&"Tutorial", &"state") == Bp.TUTORIAL_RUNNING
	var ghost: Variant = e.call(&"ghost")
	if ghost is Dictionary:
		# Why a ghost cannot go where it is shows beside the cursor (`_draw_gizmos`); the hint keeps the keys.
		_hint.text = tr(&"editor_shell.hint.place")
		_hint_panel.visible = ship and not tutorial
		return
	if tutorial:
		_hint_panel.visible = false
		return
	match int(e.call(&"tool")):
		Bp.TOOL_HULL_HANDLES:
			_hint.text = tr(&"editor_shell.hint.handles")
		Bp.TOOL_PLANES:
			_hint.text = tr(&"editor_shell.hint.planes")
		Bp.TOOL_ARMOUR:
			_hint.text = tr(&"editor_shell.hint.armour")
		Bp.TOOL_PAINT:
			_hint.text = tr(&"editor_shell.hint.paint")
		Bp.TOOL_MARKING:
			_hint.text = tr(&"editor_shell.hint.marking")
		Bp.TOOL_SCULPT:
			_hint.text = tr(&"editor_shell.hint.sculpt")
		Bp.TOOL_ROUTE:
			_hint.text = tr(&"editor_shell.hint.route_second") if _route_first >= 0 else tr(&"editor_shell.hint.route_first")
		_:
			_hint.text = tr(&"editor_shell.hint.selected") if _has_selection() else tr(&"editor_shell.hint.select")


## A short message in the hint line and the top bar (a refusal, a reason), until the next action.
func _say(message: Variant) -> void:
	_message = message
	_status.text = _Text.resolve(message)
	_refresh_hint()


func _clear_message() -> void:
	if not _Text.is_empty(_message):
		_message = &""
		_status.text = ""
		_refresh_hint()


# --- top bar actions ------------------------------------------------------------------------------

func _commit_name() -> void:
	var e := _editor()
	if e == null or not _is_open():
		return
	var text := _name.text.strip_edges()
	var current := Kit.get_str(Kit.as_dict(e.call(&"info")), "name")
	if text.is_empty():
		_name.text = current
		return
	if text != current and _allowed("editor.name"):
		_name_stale = true
		e.call(&"set_design_name", text)


func _run_undo(redo: bool) -> void:
	var e := _editor()
	if e == null or not _allowed("editor.redo" if redo else "editor.undo"):
		return
	_end_drag(true)
	_name_stale = true
	if bool(e.call(&"redo" if redo else &"undo")):
		_clear_message()
	else:
		_say(&"editor_shell.redo.none" if redo else &"editor_shell.undo.none")


func _on_save() -> void:
	var e := _editor()
	if e == null or not _is_open() or not _allowed("editor.save"):
		return
	_end_drag(false)
	_commit_name()
	var path := String(e.call(&"design_path"))
	var ok := false
	if path.begins_with("user://"):
		ok = bool(e.call(&"save_design"))
	else:
		ok = bool(e.call(&"save_design_as", _free_file_name(Kit.get_str(Kit.as_dict(e.call(&"info")), "name"))))
	if not ok:
		Screens.open_message(&"editor_shell.save.failed_title", &"editor_shell.save.failed_body")


## A file name under user://blueprints/ for a design name that is not taken yet: "Name", "Name 2"...
func _free_file_name(design_name: String) -> String:
	var stem := design_name.strip_edges().validate_filename().strip_edges()
	if stem.is_empty():
		stem = tr(&"editor_shell.file.default_name").validate_filename()
	var e := _editor()
	for n in range(1, 1000):
		var file := (stem if n == 1 else "%s %d" % [stem, n]) + ".bp.ron"
		if e == null or Kit.as_dict(e.call(&"file_info", "user://blueprints/" + file)).is_empty():
			return file
	return "%s %d.bp.ron" % [stem, Time.get_ticks_msec()]


func _on_saved(_path: String) -> void:
	_name_stale = true
	_mark_chrome()
	if not _in_splash and not _quiet_save:
		Screens.toast(&"editor_shell.saved")


func _on_file_menu(id: int) -> void:
	match id:
		1:
			_open_save_as()
		2:
			_confirm_discard(_open_browser)
		3:
			Screens.push(&"share_dialog")
		5:
			# The share dialog's import reads the clipboard into its field, and its Open in dry dock asks
			# before replacing unsaved work.
			Screens.push(&"share_dialog", {"mode": &"import", "text": _clipboard_share_string(), "folder": "Imported"})


## The clipboard when it holds a share string, so Import opens with it pasted; else empty.
func _clipboard_share_string() -> String:
	var text := DisplayServer.clipboard_get().strip_edges()
	return text if bool(Kit.call_on(&"Editor", &"is_share_string", [text])) else ""


func _open_browser() -> void:
	Screens.push(&"blueprint_browser", {"mode": "open"})


func _on_new_design(id: int) -> void:
	var menu := _file.get_popup().get_node_or_null(^"Starters") as PopupMenu
	_new_path = ""
	if menu != null and id > 0:
		_new_path = String(menu.get_item_metadata(menu.get_item_index(id)))
	_confirm_discard(_start_new_design)


func _start_new_design() -> void:
	_name_stale = true
	var issues: Variant = Kit.call_on(&"Editor", &"new_design", [_new_path, NEW_HULL_M])
	if _has_fatal(issues):
		Screens.open_message(&"editor_shell.new.failed_title", Kit.problems_body(Kit.problem_texts(issues, &"editor_shell.new.failed_body")))
		return
	_frame_view(&"all")


static func _has_fatal(issues: Variant) -> bool:
	for i: Dictionary in Kit.dicts(issues):
		if Kit.get_bool(i, "fatal"):
			return true
	return false


## Ask before throwing away unsaved work, then run `then`.
func _confirm_discard(then: Callable) -> void:
	var e := _editor()
	if e == null or not bool(e.call(&"is_dirty")):
		then.call()
		return
	_pending_discard = then
	var dialog: Node = Screens.open_dialog(&"editor_shell.discard.title", &"editor_shell.discard.body", [
		{"id": &"stay", "label": &"editor_shell.discard.stay"},
		{"id": &"save", "label": &"editor_shell.discard.save"},
		{"id": &"discard", "label": &"editor_shell.discard.discard"},
	], &"stay", &"stay")
	if dialog != null:
		dialog.connect(&"chosen", _on_discard_chosen)


func _on_discard_chosen(choice: StringName) -> void:
	var then := _pending_discard
	_pending_discard = Callable()
	if not then.is_valid():
		return
	if choice == &"save":
		_on_save()
		if not bool(Kit.call_on(&"Editor", &"is_dirty")):
			then.call()
	elif choice == &"discard":
		then.call()


func _open_save_as() -> void:
	var e := _editor()
	if e == null or not _is_open() or not _allowed("editor.save"):
		return
	var popup := PopupPanel.new()
	popup.name = &"SaveAs"
	var margin := MarginContainer.new()
	Ui.set_margins(margin, 3)
	var box := Kit.vbox(2)
	margin.add_child(box)
	popup.add_child(margin)
	box.add_child(Kit.label("editor_shell.save_as.title", &"PanelTitle"))
	var field := LineEdit.new()
	field.custom_minimum_size = Vector2(420.0, 48.0)
	field.text = Kit.get_str(Kit.as_dict(e.call(&"info")), "name")
	field.placeholder_text = tr(&"editor_shell.name.placeholder")
	field.accessibility_name = tr(&"editor_shell.save_as.field")
	field.max_length = 64
	box.add_child(field)
	var row := Kit.hbox(2)
	var ok := Kit.inline_button("editor_shell.save_as.confirm")
	var cancel := Kit.inline_button("editor_shell.save_as.cancel")
	row.add_child(ok)
	row.add_child(cancel)
	box.add_child(row)
	add_child(popup)
	ok.pressed.connect(_save_as_submit.bind(popup, field))
	field.text_submitted.connect(func(_t: String) -> void: _save_as_submit(popup, field))
	cancel.pressed.connect(popup.queue_free)
	popup.popup_hide.connect(popup.queue_free)
	popup.popup_centered()
	field.grab_focus.call_deferred()
	field.select_all.call_deferred()


func _save_as_submit(popup: PopupPanel, field: LineEdit) -> void:
	var stem := field.text.strip_edges().validate_filename().strip_edges()
	if stem.is_empty():
		return
	var e := _editor()
	var saved := e != null and bool(e.call(&"save_design_as", stem + ".bp.ron"))
	popup.queue_free()
	if not saved:
		Screens.open_message(&"editor_shell.save.failed_title", &"editor_shell.save.failed_body")


func _on_leave() -> void:
	if not _allowed("menu.leave_dock"):
		return
	_end_drag(true)
	_commit_name()
	# A running tutorial asks its own question (leave the tutorial?) rather than offering to save its hull.
	var overlay := get_node_or_null(^"TutorialOverlay")
	if _dock_mode() == Bp.DOCK_TUTORIAL and overlay != null and overlay.has_method(&"ask_skip") \
			and Kit.call_on(&"Tutorial", &"state") == Bp.TUTORIAL_RUNNING:
		overlay.call(&"ask_skip")
		return
	_confirm_discard(_close_and_route)


## Close the dock and go back where it was opened from. The campaign moves its own phase once the editor
## reports closed and the router follows; a lobby dock returns to the lobby.
func _close_and_route() -> void:
	var mode := _dock_mode()
	var e := _editor()
	if e != null:
		e.call(&"close")
	match mode:
		Bp.DOCK_CAMPAIGN:
			pass
		Bp.DOCK_LOBBY:
			Screens.change(&"lobby")
		Bp.DOCK_TUTORIAL:
			Kit.call_on(&"Tutorial", &"stop")
			Screens.change(&"main_menu")
		_:
			Screens.change(&"main_menu")


## Launch (L188): only validation blocks it. Campaign: commit the refit and close; the briefing's own Launch
## starts the sortie. Lobby: field the design and return to the lobby. Sandbox and enemy class: the sandbox
## setup opens with this design picked.
func _on_launch() -> void:
	var e := _editor()
	if e == null or _launch.disabled or not _allowed("editor.launch"):
		return
	_end_drag(false)
	_commit_name()
	if not bool(e.call(&"launch")):
		_say(_refusal(&"editor_shell.launch.failed"))
		return
	match _dock_mode():
		Bp.DOCK_CAMPAIGN:
			if _refit_has_changes() and Kit.call_on(&"Campaign", &"commit_refit") != true:
				Screens.open_message(&"editor_shell.refit.failed_title", &"editor_shell.refit.failed_body")
				return
			e.call(&"close")
		Bp.DOCK_LOBBY:
			var issues: Variant = Kit.call_on(&"Lobby", &"select_blueprint", [String(e.call(&"design_path"))])
			if not Kit.dicts(issues).is_empty():
				Screens.open_message(&"editor_shell.lobby.failed_title", Kit.problems_body(Kit.problem_texts(issues, &"editor_shell.lobby.failed_body")))
				return
			e.call(&"close")
			Screens.change(&"lobby")
		_:
			DesignPicker.last_path = String(e.call(&"design_path"))
			e.call(&"close")
			Screens.change(&"sandbox_setup")


func _refit_has_changes() -> bool:
	var diff := Kit.as_dict(Kit.call_on(&"Editor", &"refit_diff"))
	var changes: Variant = diff.get("changes", [])
	return (changes is Array and not (changes as Array).is_empty()) or absf(Kit.get_float(diff, "structure_delta_t")) > 0.0


## Why the last splash or launch refused (`Editor.refusal`), or `fallback`.
func _refusal(fallback: StringName) -> Variant:
	var why: Variant = Kit.call_on(&"Editor", &"refusal")
	if why is Dictionary and not _Text.is_empty(why):
		return why
	return fallback


# --- symmetry, slice, camera ----------------------------------------------------------------------

func _on_symmetry_picked(index: int) -> void:
	var e := _editor()
	if e == null:
		return
	if _allowed("editor.symmetry"):
		e.call(&"set_symmetry", _symmetry.get_item_id(index), int(_radial.value))
	_refresh_symmetry()


func _on_radial_changed(_value: float) -> void:
	_on_symmetry_picked(_symmetry.selected)


func _toggle_mirror() -> void:
	var e := _editor()
	if e == null or not _allowed("editor.symmetry"):
		return
	var s := Kit.as_dict(e.call(&"symmetry"))
	var on := Kit.get_int(s, "symmetry") == Bp.SYMMETRY_MIRROR
	e.call(&"set_symmetry", Bp.SYMMETRY_NONE if on else Bp.SYMMETRY_MIRROR, Kit.get_int(s, "radial_count", 2))
	_refresh_symmetry()
	_say(&"editor_shell.symmetry.mirror_off" if on else &"editor_shell.symmetry.mirror_on")


func _on_slice_picked(index: int) -> void:
	if _view != null:
		_view.call(&"set_deck_slice", index - 1)
	_refresh_handles()


func _on_camera_menu(id: int) -> void:
	var menu := _camera_menu.get_popup()
	_frame_view(StringName(menu.get_item_metadata(menu.get_item_index(id))))


func _frame_view(what: StringName) -> void:
	if _view == null:
		return
	if what == &"selection" and not _has_selection():
		what = &"all"
	_view.call(&"frame", what)
	_refresh_handles()


func _has_selection() -> bool:
	var e := _editor()
	if e == null:
		return false
	var parts: Variant = Kit.as_dict(e.call(&"selection")).get("parts", PackedInt32Array())
	return parts is PackedInt32Array and not (parts as PackedInt32Array).is_empty()


## Show arcs and blind sectors of the inspected parts only.
func _focus_arcs(slots: PackedInt32Array) -> void:
	if _view != null:
		_view.call(&"set_overlay_focus", slots)
	if not slots.is_empty():
		_overlays.call(&"ensure", Bp.OVERLAY_ARCS | Bp.OVERLAY_BLIND_SECTORS)


# --- tools ----------------------------------------------------------------------------------------

## The left tab sets the tool family: Parts and Officers select, Hull and Paint use their panel's mode.
func _on_left_tab(tab: int) -> void:
	var e := _editor()
	if e == null or not _is_open():
		return
	_end_drag(false)
	_clear_message()
	match tab:
		LEFT_HULL:
			_set_tool(int(_hull.call(&"tool")))
		LEFT_PAINT:
			_set_tool(int(_paint.call(&"tool")))
		_:
			var t := _tool()
			if t != Bp.TOOL_PLACE_PART and t != Bp.TOOL_PLACE_KIT and t != Bp.TOOL_ROUTE:
				_set_tool(Bp.TOOL_SELECT)


func _set_tool(tool: int) -> void:
	var e := _editor()
	if e == null:
		return
	if int(e.call(&"tool")) != tool:
		e.call(&"set_tool", tool)
	_refresh_handles()
	_refresh_hint()


func _on_tool_changed(tool: int) -> void:
	_clear_message()
	if tool != Bp.TOOL_ROUTE:
		_route_first = -1
	var want := -1
	match tool:
		Bp.TOOL_HULL_HANDLES, Bp.TOOL_PLANES, Bp.TOOL_SCULPT:
			want = LEFT_HULL
		Bp.TOOL_ARMOUR, Bp.TOOL_PAINT, Bp.TOOL_MARKING:
			want = LEFT_PAINT
		Bp.TOOL_PLACE_PART, Bp.TOOL_PLACE_KIT:
			want = LEFT_PARTS
	if tool == Bp.TOOL_PLANES:
		# Decks and bulkheads are drawn, and picked before hull cells, only with Compartments on.
		_overlays.call(&"ensure", Bp.OVERLAY_COMPARTMENTS)
	if want >= 0 and _left_tabs.current_tab != want:
		_left_tabs.set_block_signals(true)
		_left_tabs.current_tab = want
		_left_tabs.set_block_signals(false)
	_refresh_handles()
	_refresh_hint()


func _on_selection_changed() -> void:
	_clear_message()
	_refresh_hint()
	_gizmos.queue_redraw()


func _on_ghost_changed() -> void:
	_refresh_hint()
	_gizmos.queue_redraw()


func _on_tray_picked(_id: StringName, _is_kit: bool) -> void:
	_clear_message()
	if _hover.x >= 0.0 and _has_ghost():
		_move_ghost(_hover)
	_refresh_hint()


func _start_route(slot: int) -> void:
	_set_tool(Bp.TOOL_ROUTE)
	_route_first = slot
	_refresh_hint()


# --- view input -----------------------------------------------------------------------------------

func _on_view_exit() -> void:
	_hover = Vector2(-1.0, -1.0)
	if _hover_handle != "":
		_hover_handle = ""
		_gizmos.queue_redraw()


func _on_view_input(event: InputEvent) -> void:
	if _in_splash or not _is_open() or _view == null:
		return
	if event is InputEventPanGesture:
		var g := event as InputEventPanGesture
		var command := g.meta_pressed if OS.get_name() == "macOS" else g.ctrl_pressed
		_view.call(&"pan_gesture", g.delta, g.shift_pressed, command, g.position)
		_after_camera()
		_display.accept_event()
	elif event is InputEventMagnifyGesture:
		var m := event as InputEventMagnifyGesture
		_view.call(&"magnify_gesture", m.factor, m.position)
		_after_camera()
		_display.accept_event()
	elif event is InputEventMouseButton:
		_on_view_button(event as InputEventMouseButton)
		_display.accept_event()
	elif event is InputEventMouseMotion:
		_on_view_motion(event as InputEventMouseMotion)
		_display.accept_event()


static func _secondary(mb: InputEventMouseButton) -> bool:
	if mb.button_index == MOUSE_BUTTON_RIGHT:
		return true
	# Ctrl-click is the macOS secondary click (L245); Ctrl is never a gesture modifier there.
	return mb.button_index == MOUSE_BUTTON_LEFT and mb.ctrl_pressed and OS.get_name() == "macOS"


func _on_view_button(mb: InputEventMouseButton) -> void:
	var pos := mb.position
	if mb.pressed and not _display.has_focus():
		_display.grab_focus()
	match mb.button_index:
		MOUSE_BUTTON_WHEEL_UP, MOUSE_BUTTON_WHEEL_DOWN:
			if mb.pressed:
				var f := WHEEL_ZOOM if mb.button_index == MOUSE_BUTTON_WHEEL_UP else 1.0 / WHEEL_ZOOM
				if mb.factor > 0.0 and not is_equal_approx(mb.factor, 1.0):
					f = pow(f, clampf(mb.factor, 0.1, 4.0))
				_view.call(&"zoom", f, pos)
				_after_camera()
			return
		MOUSE_BUTTON_WHEEL_LEFT, MOUSE_BUTTON_WHEEL_RIGHT:
			return
		MOUSE_BUTTON_MIDDLE:
			if mb.pressed:
				_begin_camera_drag(Drag.PAN if mb.shift_pressed else Drag.ORBIT, pos)
			elif _drag == Drag.ORBIT or _drag == Drag.PAN:
				_drag = Drag.NONE
			return
	if _secondary(mb):
		# Actions fire on release (L1717).
		if not mb.pressed:
			if _drag != Drag.NONE:
				_end_drag(true)
			_open_context(pos)
		return
	if mb.button_index != MOUSE_BUTTON_LEFT:
		return
	if mb.pressed:
		if mb.alt_pressed:
			_begin_camera_drag(Drag.PAN if mb.shift_pressed else Drag.ORBIT, pos)
			return
		if mb.double_click and _drag == Drag.NONE and not _has_ghost():
			if StringName(_pick(pos).get("kind", &"none")) == &"part":
				_frame_view(&"selection")
			return
		_begin_press(pos, mb.shift_pressed)
	else:
		_release(pos)


func _on_view_motion(mm: InputEventMouseMotion) -> void:
	var pos := mm.position
	_hover = pos
	match _drag:
		Drag.ORBIT:
			_view.call(&"orbit", pos - _drag_last, _drag_start)
			_drag_last = pos
			_after_camera()
			return
		Drag.PAN:
			_view.call(&"pan", pos - _drag_last)
			_drag_last = pos
			_after_camera()
			return
		Drag.PRESS:
			if pos.distance_to(Vector2(_press.get("pos", pos))) >= DRAG_PX:
				_promote_press(pos)
			return
		Drag.HANDLE:
			_drag_handle(pos, mm.shift_pressed)
			return
		Drag.PLANE:
			_drag_plane(pos)
			return
		Drag.MOVE:
			_drag_move(pos)
			return
		Drag.BOX:
			_box = Rect2(_drag_start, Vector2.ZERO).expand(pos)
			_gizmos.queue_redraw()
			return
		Drag.STROKE:
			_stroke_points.append(pos)
			set_process(true)
			return
	if _has_ghost():
		_move_ghost(pos)
		return
	if _tool() == Bp.TOOL_HULL_HANDLES and not _handles.is_empty():
		var near := _handle_near(pos)
		if near != _hover_handle:
			_hover_handle = near
			_gizmos.queue_redraw()


func _begin_camera_drag(kind: int, pos: Vector2) -> void:
	_end_drag(false)
	_drag = kind
	_drag_start = pos
	_drag_last = pos


func _after_camera() -> void:
	_refresh_handles()


func _tool() -> int:
	var e := _editor()
	return int(e.call(&"tool")) if e != null else Bp.TOOL_SELECT


func _has_ghost() -> bool:
	var e := _editor()
	return e != null and e.call(&"ghost") is Dictionary


func _pick(pos: Vector2) -> Dictionary:
	if _view == null:
		return {}
	return Kit.as_dict(_view.call(&"pick", pos))


func _move_ghost(pos: Vector2) -> void:
	var cell: Variant = _view.call(&"cell_at", pos)
	if cell is Vector3i:
		Kit.call_on(&"Editor", &"ghost_move", [cell])
		_refresh_hint()
	_gizmos.queue_redraw()


func _begin_press(pos: Vector2, shift: bool) -> void:
	_end_drag(false)
	_clear_message()
	_drag_start = pos
	_drag_last = pos
	if _has_ghost():
		_press = {"pos": pos, "ghost": true, "shift": shift, "hit": {}}
		_drag = Drag.PRESS
		return
	var hit := _pick(pos)
	var kind := StringName(hit.get("kind", &"none"))
	var tool := _tool()
	_press = {"pos": pos, "shift": shift, "hit": hit, "ghost": false}
	match tool:
		Bp.TOOL_HULL_HANDLES:
			if kind == &"handle":
				_begin_handle(hit)
				return
		Bp.TOOL_PLANES:
			if kind == &"deck" or kind == &"bulkhead":
				_begin_plane(hit, pos)
				return
		Bp.TOOL_ARMOUR, Bp.TOOL_PAINT, Bp.TOOL_SCULPT:
			_begin_stroke(tool, pos)
			return
		Bp.TOOL_MARKING:
			_place_marking(hit)
			_press = {}
			return
		Bp.TOOL_ROUTE:
			_route_click(hit)
			_press = {}
			return
	_drag = Drag.PRESS


## A press became a drag: on a part it moves the selection, elsewhere it draws a box.
func _promote_press(pos: Vector2) -> void:
	if bool(_press.get("ghost", false)):
		_drag = Drag.NONE
		_press = {}
		return
	var hit: Dictionary = _press.get("hit", {})
	var start := Vector2(_press.get("pos", pos))
	if StringName(hit.get("kind", &"none")) == &"part" and _allowed("editor.select"):
		_begin_move(hit, start)
		_drag_move(pos)
	else:
		_drag = Drag.BOX
		_drag_start = start
		_box = Rect2(start, Vector2.ZERO).expand(pos)
		_gizmos.queue_redraw()


func _release(_pos: Vector2) -> void:
	var e := _editor()
	match _drag:
		Drag.PRESS:
			_drag = Drag.NONE
			if e != null:
				if bool(_press.get("ghost", false)):
					_place_ghost()
				elif _allowed("editor.select"):
					var hit: Dictionary = _press.get("hit", {})
					var shift := bool(_press.get("shift", false))
					if StringName(hit.get("kind", &"none")) == &"part":
						e.call(&"select_parts", PackedInt32Array([int(hit.get("slot", -1))]), shift)
					elif not shift:
						e.call(&"clear_selection")
		Drag.BOX:
			_drag = Drag.NONE
			if e != null and _allowed("editor.select"):
				var slots: Variant = _view.call(&"parts_in_rect", _box.abs())
				var shift := bool(_press.get("shift", false))
				if slots is PackedInt32Array and not (slots as PackedInt32Array).is_empty():
					e.call(&"select_parts", slots, shift)
				elif not shift:
					e.call(&"clear_selection")
			_box = Rect2()
			_gizmos.queue_redraw()
		Drag.HANDLE, Drag.PLANE, Drag.MOVE, Drag.STROKE:
			_end_drag(false)
		Drag.ORBIT, Drag.PAN:
			_drag = Drag.NONE
	_press = {}
	_refresh_hint()


func _place_ghost() -> void:
	if not _allowed("editor.tray.place"):
		return
	var e := _editor()
	var placed: Variant = e.call(&"ghost_place")
	if placed is PackedInt32Array and (placed as PackedInt32Array).is_empty():
		var g: Variant = e.call(&"ghost")
		if g is Dictionary and not _Text.is_empty((g as Dictionary).get("reason", {})):
			_say((g as Dictionary).get("reason", {}))
		else:
			_say(&"editor_shell.place.refused")


## Close whatever drag is running. `cancel` puts the design back as it was before the drag. Every group a
## drag opened is closed here, so Esc, focus loss, Splash and closing the dock never leave one open.
func _end_drag(cancel: bool) -> void:
	var e := _editor()
	match _drag:
		Drag.HANDLE, Drag.PLANE, Drag.MOVE:
			if e != null:
				e.call(&"end_group", cancel)
		Drag.STROKE:
			if not cancel:
				_flush_stroke()
			if e != null:
				e.call(&"end_group", cancel)
			if not cancel:
				_paint.call(&"show_stroke", _stroke_total)
			_stroke_points.clear()
			_stroke_seen.clear()
			_stroke_total = {}
			set_process(false)
		Drag.BOX:
			_box = Rect2()
			_gizmos.queue_redraw()
	_drag = Drag.NONE
	_handle_drag = {}
	_plane_drag = {}
	_move_drag = {}
	_press = {}
	if _after_drag:
		_after_drag = false
		_mark_chrome()
		_mark_launch()
		_refresh_slices()


# --- handles --------------------------------------------------------------------------------------

func _begin_handle(hit: Dictionary) -> void:
	var id := String(hit.get("handle", ""))
	var station := int(hit.get("station", 0))
	if id.is_empty() or not _allowed("editor.handle." + id):
		return
	var info: Dictionary = _handle_values.get("%s:%d" % [id, station], {})
	if info.is_empty():
		return
	_handle_drag = {"id": id, "station": station, "value": float(info.get("value", 0.0)), "min": float(info.get("min", 0.0)),
		"max": float(info.get("max", 1.0)), "step": float(info.get("step", 0.0)), "acc": 0.0}
	Kit.call_on(&"Editor", &"begin_group", [&"dock.undo.handle"])
	_drag = Drag.HANDLE
	_hull.call(&"focus_station", station)
	_gizmos.queue_redraw()


## Right and up raise a handle, left and down lower it; the whole range takes HANDLE_SWEEP_PX.
func _drag_handle(pos: Vector2, fine: bool) -> void:
	var d := pos - _drag_last
	_drag_last = pos
	var span := float(_handle_drag["max"]) - float(_handle_drag["min"])
	if span <= 0.0:
		return
	_handle_drag["acc"] = float(_handle_drag["acc"]) + (d.x - d.y) * (0.1 if fine else 1.0)
	var v := float(_handle_drag["value"]) + float(_handle_drag["acc"]) * span / HANDLE_SWEEP_PX
	var step := float(_handle_drag["step"])
	if step > 0.0:
		v = snappedf(v, step)
	v = clampf(v, float(_handle_drag["min"]), float(_handle_drag["max"]))
	Kit.call_on(&"Editor", &"set_handle", [_handle_drag["id"], _handle_drag["station"], v, true])


func _refresh_handles() -> void:
	_handles = []
	_handle_values = {}
	if _view != null and _is_open() and _mode == Mode.SHIP and not _in_splash and _tool() == Bp.TOOL_HULL_HANDLES:
		for h: Dictionary in Kit.dicts(Kit.call_on(&"Editor", &"handles")):
			_handle_values["%s:%d" % [String(h.get("id", "")), int(h.get("station", 0))]] = h
		for p: Dictionary in Kit.dicts(_view.call(&"handle_points")):
			if bool(p.get("visible", false)):
				_handles.append(p)
	_gizmos.queue_redraw()


func _handle_near(pos: Vector2) -> String:
	var best := ""
	var best_d := HANDLE_PICK_PX
	for h: Dictionary in _handles:
		var d := pos.distance_to(Vector2(h.get("screen", Vector2.ZERO)))
		if d < best_d:
			best_d = d
			best = "%s:%d" % [String(h.get("id", "")), int(h.get("station", 0))]
	return best


func _draw_gizmos() -> void:
	var ring := Ui.colour(&"text.primary")
	var hot := Ui.colour(&"select")
	var inset := Ui.colour(&"surface.inset")
	var focus_station := int(_hull.call(&"current_station"))
	var dragged := "%s:%d" % [String(_handle_drag.get("id", "")), int(_handle_drag.get("station", -1))]
	for h: Dictionary in _handles:
		var station := int(h.get("station", 0))
		var key := "%s:%d" % [String(h.get("id", "")), station]
		var at := Vector2(h.get("screen", Vector2.ZERO))
		var is_hot := (_drag == Drag.HANDLE and key == dragged) or key == _hover_handle
		var r := HANDLE_RING_HOT if is_hot else HANDLE_RING
		_gizmos.draw_circle(at, r + 2.0, inset)
		_gizmos.draw_arc(at, r, 0.0, TAU, 24, hot if is_hot else ring, 3.0 if is_hot else 2.0, true)
		if is_hot or station == focus_station:
			_gizmos.draw_circle(at, 3.0, hot if is_hot else ring)
	# The handle under the cursor or in the drag says what it is and its value, so the rings can be told
	# apart without the Hull tab.
	var named := dragged if _drag == Drag.HANDLE else _hover_handle
	if _handle_values.has(named):
		for h: Dictionary in _handles:
			if "%s:%d" % [String(h.get("id", "")), int(h.get("station", 0))] == named:
				_draw_tag(Vector2(h.get("screen", Vector2.ZERO)) + Vector2(HANDLE_RING_HOT + 8.0, -HANDLE_RING_HOT - 8.0), _handle_text(_handle_values[named]), null)
				break
	# A ghost that cannot go where it is says why next to the cursor, where the eye is.
	if _drag == Drag.NONE and _hover.x >= 0.0:
		var g: Variant = Kit.call_on(&"Editor", &"ghost")
		if g is Dictionary and not Kit.get_bool(g as Dictionary, "valid", true):
			var why := _Text.resolve((g as Dictionary).get("reason", {}))
			if not why.is_empty():
				_draw_tag(_hover + Vector2(20.0, 20.0), why, Ui.icon(&"check_fail"))
	if _drag == Drag.BOX and _box.size.length() > 0.0:
		_gizmos.draw_rect(_box.abs(), Ui.colour(&"ui.box"), false, 2.0)


## "Beam 4.7 m, station 2": a handle's field name, value and station.
func _handle_text(h: Dictionary) -> String:
	var id := String(h.get("id", ""))
	var unit := StringName(h.get("unit", &"none"))
	var text := tr(&"editor_shell.handle.value").format([tr(StringName("editor_hull.field." + id)),
		Kit.number(float(h.get("value", 0.0)), unit, Kit.decimals_for(float(h.get("step", 0.1))))])
	if id in STATION_HANDLE_IDS:
		text = tr(&"editor_shell.handle.station").format([text, Kit.number(float(int(h.get("station", 0)) + 1), &"none", 0)])
	return text


## One line of text on an opaque tag in the view, kept inside it: a handle's name and value, or why the ghost
## cannot go where it is (with the Fail cross, so the reason never rests on colour).
func _draw_tag(at: Vector2, text: String, icon: Variant) -> void:
	var font := _gizmos.get_theme_font(&"font", &"Dense")
	var size := _gizmos.get_theme_font_size(&"font_size", &"Dense")
	var pad := float(Ui.spacing(1))
	var icon_px := 0.0
	if icon is Texture2D:
		icon_px = float(size) + pad * 0.5
	var text_size := font.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, size)
	var box := Rect2(at, Vector2(text_size.x + icon_px + pad * 2.0, font.get_height(size) + pad))
	var bounds := Rect2(Vector2.ZERO, _gizmos.size)
	box.position.x = clampf(box.position.x, 0.0, maxf(bounds.size.x - box.size.x, 0.0))
	box.position.y = clampf(box.position.y, 0.0, maxf(bounds.size.y - box.size.y, 0.0))
	_gizmos.draw_rect(box, Ui.colour(&"surface.raised"))
	_gizmos.draw_rect(box, Ui.colour(&"rule.divider"), false, 1.0)
	var x := box.position.x + pad
	if icon is Texture2D:
		var s := float(size)
		_gizmos.draw_texture_rect(icon as Texture2D, Rect2(Vector2(x, box.position.y + (box.size.y - s) * 0.5), Vector2(s, s)), false, Ui.colour(&"alert.warning"))
		x += icon_px
	_gizmos.draw_string(font, Vector2(x, box.position.y + pad * 0.5 + font.get_ascent(size)), text, HORIZONTAL_ALIGNMENT_LEFT, -1, size, Ui.colour(&"text.primary"))


# --- planes ---------------------------------------------------------------------------------------

func _begin_plane(hit: Dictionary, pos: Vector2) -> void:
	if not _allowed("editor.hull.planes"):
		return
	var axis := StringName(hit.get("kind", &"deck"))
	var index := int(hit.get("plane", -1))
	var hull := Kit.as_dict(Kit.call_on(&"Editor", &"hull"))
	var list: Variant = hull.get("decks" if axis == &"deck" else "bulkheads", PackedFloat32Array())
	if index < 0 or not list is PackedFloat32Array or index >= (list as PackedFloat32Array).size():
		return
	var at := Vector3(hit.get("pos", Vector3.ZERO))
	var start := float((list as PackedFloat32Array)[index])
	_plane_drag = {"axis": axis, "index": index, "start": start, "at": at, "hit": _plane_hit(axis, at, pos), "applied": start}
	Kit.call_on(&"Editor", &"begin_group", [&"dock.undo.plane_move"])
	_drag = Drag.PLANE


## Where the cursor ray meets the surface a plane slides on: a deck moves up and down on the vertical plane
## through the picked point facing the camera; a bulkhead moves fore and aft on the horizontal one.
func _plane_hit(axis: StringName, at: Vector3, pos: Vector2) -> Variant:
	if _camera == null:
		return null
	var normal := Vector3.UP
	if axis == &"deck":
		var f := -_camera.global_transform.basis.z
		normal = Vector3(f.x, 0.0, f.z)
		normal = normal.normalized() if normal.length() > 0.01 else Vector3.BACK
	return Plane(normal, at).intersects_ray(_camera.project_ray_origin(pos), _camera.project_ray_normal(pos))


func _drag_plane(pos: Vector2) -> void:
	var axis := StringName(_plane_drag["axis"])
	var hit: Variant = _plane_hit(axis, Vector3(_plane_drag["at"]), pos)
	var start: Variant = _plane_drag["hit"]
	if not hit is Vector3 or not start is Vector3:
		return
	var delta := (hit as Vector3) - (start as Vector3)
	var want := snappedf(float(_plane_drag["start"]) + (delta.y if axis == &"deck" else delta.x), CELL_M)
	if is_equal_approx(want, float(_plane_drag["applied"])):
		return
	var got: Variant = Kit.call_on(&"Editor", &"move_plane", [axis, int(_plane_drag["index"]), want])
	if got is float:
		_plane_drag["applied"] = got


# --- moving the selection -------------------------------------------------------------------------

func _begin_move(hit: Dictionary, start: Vector2) -> void:
	var e := _editor()
	var slot := int(hit.get("slot", -1))
	var parts: Variant = Kit.as_dict(e.call(&"selection")).get("parts", PackedInt32Array())
	if not parts is PackedInt32Array or not (parts as PackedInt32Array).has(slot):
		e.call(&"select_parts", PackedInt32Array([slot]), false)
	var at := Vector3(hit.get("pos", Vector3.ZERO))
	var origin_hit: Variant = _ground_hit(at.y, start)
	if not origin_hit is Vector3:
		_drag = Drag.NONE
		return
	_move_drag = {"y": at.y, "hit": origin_hit, "applied": Vector3i.ZERO}
	e.call(&"begin_group", &"dock.undo.move")
	_drag = Drag.MOVE


func _ground_hit(y: float, pos: Vector2) -> Variant:
	if _camera == null:
		return null
	return Plane(Vector3.UP, y).intersects_ray(_camera.project_ray_origin(pos), _camera.project_ray_normal(pos))


func _drag_move(pos: Vector2) -> void:
	if _move_drag.is_empty():
		return
	var hit: Variant = _ground_hit(float(_move_drag["y"]), pos)
	if not hit is Vector3:
		return
	var d := ((hit as Vector3) - Vector3(_move_drag["hit"])) / CELL_M
	var want := Vector3i(roundi(d.x), 0, roundi(d.z))
	var applied := Vector3i(_move_drag["applied"])
	if want == applied:
		return
	# Step a cell at a time toward the cursor, so the selection slides up against whatever stops it
	# instead of refusing the whole jump.
	var blocked := false
	for i in 64:
		if applied == want:
			break
		var step := Vector3i(signi(want.x - applied.x), 0, 0)
		if step == Vector3i.ZERO:
			step = Vector3i(0, 0, signi(want.z - applied.z))
		if not bool(Kit.call_on(&"Editor", &"move_selection", [step])):
			var other := Vector3i(0, 0, signi(want.z - applied.z)) if step.x != 0 else Vector3i.ZERO
			if other == Vector3i.ZERO or not bool(Kit.call_on(&"Editor", &"move_selection", [other])):
				blocked = true
				break
			step = other
		applied += step
	_move_drag["applied"] = applied
	if blocked:
		_say(&"editor_shell.move.blocked")
	else:
		_clear_message()


# --- strokes, sculpting, markings, links ----------------------------------------------------------

func _begin_stroke(tool: int, pos: Vector2) -> void:
	var ids: Dictionary[int, String] = {Bp.TOOL_ARMOUR: "editor.paint.armour", Bp.TOOL_PAINT: "editor.paint.colour", Bp.TOOL_SCULPT: "editor.hull.sculpt"}
	if not _allowed(ids.get(tool, "editor.paint")):
		return
	var labels: Dictionary[int, StringName] = {Bp.TOOL_ARMOUR: &"dock.undo.armour", Bp.TOOL_PAINT: &"dock.undo.paint", Bp.TOOL_SCULPT: &"dock.undo.sculpt"}
	Kit.call_on(&"Editor", &"begin_group", [labels.get(tool, &"dock.undo.edit")])
	_stroke_tool = tool
	_stroke_points = [pos]
	_stroke_seen = {}
	_stroke_total = {"tool": tool, "cells": 0, "mass_delta_kg": 0.0, "cost_delta": 0}
	_drag = Drag.STROKE
	_flush_stroke()


func _process(_delta: float) -> void:
	if _drag == Drag.STROKE and not _stroke_points.is_empty():
		_flush_stroke()
	else:
		set_process(false)


## Apply the cursor positions gathered since the last frame as one call, so a fast stroke costs one
## design change a frame, not one per mouse event.
func _flush_stroke() -> void:
	if _stroke_points.is_empty() or _view == null:
		return
	var points := _stroke_points.duplicate()
	_stroke_points.clear()
	var e := _editor()
	if e == null:
		return
	var cells := PackedVector3Array()
	if _stroke_tool == Bp.TOOL_SCULPT:
		var add := bool(_hull.call(&"sculpt_adds"))
		for p: Vector2 in points:
			var c: Variant = null
			if add:
				c = _view.call(&"cell_at", p)
			else:
				var hit := _pick(p)
				if StringName(hit.get("kind", &"none")) == &"cell":
					c = hit.get("cell")
			if c is Vector3i and not _stroke_seen.has(c):
				_stroke_seen[c] = true
				cells.append(Vector3(c as Vector3i))
		if cells.is_empty():
			return
		var n := int(e.call(&"sculpt", cells, add, int(_hull.call(&"sculpt_shape")), int(_hull.call(&"sculpt_orientation"))))
		_stroke_total["cells"] = int(_stroke_total["cells"]) + n
		if n == 0:
			_say(&"editor_shell.sculpt.refused")
		return
	var radius := int(_paint.call(&"brush_radius"))
	for p: Vector2 in points:
		var brush: Variant = _view.call(&"brush_cells", p, radius)
		if not brush is PackedVector3Array:
			continue
		for v: Vector3 in brush as PackedVector3Array:
			var key := Vector3i(roundi(v.x), roundi(v.y), roundi(v.z))
			if not _stroke_seen.has(key):
				_stroke_seen[key] = true
				cells.append(v)
	if cells.is_empty():
		return
	if _stroke_tool == Bp.TOOL_ARMOUR:
		var r := Kit.as_dict(e.call(&"paint_armour", cells, int(_paint.call(&"armour_tier"))))
		_stroke_total["cells"] = int(_stroke_total["cells"]) + Kit.get_int(r, "cells")
		_stroke_total["mass_delta_kg"] = float(_stroke_total["mass_delta_kg"]) + Kit.get_float(r, "mass_delta_kg")
		_stroke_total["cost_delta"] = int(_stroke_total["cost_delta"]) + Kit.get_int(r, "cost_delta")
	else:
		_stroke_total["cells"] = int(_stroke_total["cells"]) + int(e.call(&"paint_colour", cells, int(_paint.call(&"paint_index"))))
	_paint.call(&"show_stroke", _stroke_total)


func _place_marking(hit: Dictionary) -> void:
	if not _allowed("editor.paint.marking"):
		return
	if StringName(hit.get("kind", &"none")) != &"cell":
		_say(&"editor_shell.marking.miss")
		return
	var m := Kit.as_dict(_paint.call(&"marking_settings"))
	var index: Variant = Kit.call_on(&"Editor", &"add_marking", [Kit.get_int(m, "kind"), hit.get("cell", Vector3i.ZERO),
		int(hit.get("face", 0)), Kit.get_int(m, "paint"), Kit.get_int(m, "size_cells", 2), Kit.get_str(m, "text")])
	if not index is int or int(index) < 0:
		_say(&"editor_shell.marking.refused")


func _route_click(hit: Dictionary) -> void:
	if StringName(hit.get("kind", &"none")) != &"part":
		_say(&"editor_shell.route.miss")
		return
	var slot := int(hit.get("slot", -1))
	if _route_first < 0:
		_route_first = slot
		_refresh_hint()
		return
	var ok := bool(Kit.call_on(&"Editor", &"add_data_link", [_route_first, slot]))
	_route_first = -1
	_set_tool(Bp.TOOL_SELECT)
	_say(&"editor_shell.route.added" if ok else &"editor_shell.route.refused")


# --- drag and drop from the tray ------------------------------------------------------------------

func _can_drop_on_view(at: Vector2, data: Variant) -> bool:
	if not data is Dictionary or not (data as Dictionary).has("tray_id") or _in_splash:
		return false
	var d := data as Dictionary
	if not _has_ghost():
		_tray.call(&"pick", StringName(d["tray_id"]), bool(d.get("is_kit", false)))
	_hover = at
	if _has_ghost():
		_move_ghost(at)
		return true
	return false


func _drop_on_view(at: Vector2, _data: Variant) -> void:
	_move_ghost(at)
	_place_ghost()
	_display.grab_focus()


# --- keys -----------------------------------------------------------------------------------------

func _unhandled_input(event: InputEvent) -> void:
	if _in_splash or not _is_open() or not is_visible_in_tree():
		return
	if not event is InputEventKey or not event.is_pressed():
		return
	if Screens.top() != self:
		return
	if event.is_action_pressed(&"splash", false, true):
		_do_splash()
	elif event.is_action_pressed(&"editor_redo", true, true):
		_run_undo(true)
	elif event.is_action_pressed(&"editor_undo", true, true):
		_run_undo(false)
	elif event.is_action_pressed(&"editor_save", false, true):
		_on_save()
	elif event.is_action_pressed(&"editor_select_all", false, true):
		if _allowed("editor.select"):
			Kit.call_on(&"Editor", &"select_all")
	elif event.is_action_pressed(&"editor_copy", false, true):
		_say(&"editor_shell.copy.done" if bool(Kit.call_on(&"Editor", &"copy_selection")) else &"editor_shell.copy.nothing")
	elif event.is_action_pressed(&"editor_paste", false, true):
		_paste()
	elif event.is_action_pressed(&"editor_duplicate", false, true):
		if bool(Kit.call_on(&"Editor", &"copy_selection")):
			_paste()
		else:
			_say(&"editor_shell.copy.nothing")
	elif event.is_action_pressed(&"editor_delete", false, true):
		_delete_selection()
	elif event.is_action_pressed(&"editor_rotate_back", false, true):
		_rotate(-1)
	elif event.is_action_pressed(&"editor_rotate", false, true):
		_rotate(1)
	elif event.is_action_pressed(&"editor_tilt", false, true):
		_tilt()
	elif event.is_action_pressed(&"editor_mirror", false, true):
		_toggle_mirror()
	elif event.is_action_pressed(&"editor_xray", false, true):
		_overlays.call(&"toggle", Bp.OVERLAY_XRAY)
	elif event.is_action_pressed(&"editor_frame", false, true):
		_frame_view(&"selection")
	elif event.is_action_pressed(&"editor_cancel", false, true):
		_cancel()
	else:
		return
	get_viewport().set_input_as_handled()


func _paste() -> void:
	if not _allowed("editor.select"):
		return
	if bool(Kit.call_on(&"Editor", &"paste")):
		if _hover.x >= 0.0:
			_move_ghost(_hover)
		_refresh_hint()
	else:
		_say(&"editor_shell.paste.nothing")


func _delete_selection() -> void:
	if not _allowed("editor.select"):
		return
	var n := int(Kit.call_on(&"Editor", &"delete_selection"))
	if n <= 0:
		_say(&"editor_shell.delete.nothing")
	else:
		_say({"key": &"editor_shell.delete.done", "args": [n]})


func _rotate(turns: int) -> void:
	var e := _editor()
	if e == null:
		return
	if _has_ghost():
		e.call(&"ghost_rotate", turns)
		_refresh_hint()
		return
	var p := _single_selected()
	if p.is_empty():
		return
	if not bool(e.call(&"move_part", int(p["slot"]), p["cell"], posmod(int(p["yaw"]) + turns, 4), int(p["tilt"]))):
		_say(&"editor_shell.rotate.blocked")


func _tilt() -> void:
	var e := _editor()
	if e == null:
		return
	if _has_ghost():
		e.call(&"ghost_tilt")
		_refresh_hint()
		return
	var p := _single_selected()
	if p.is_empty():
		return
	if not bool(e.call(&"move_part", int(p["slot"]), p["cell"], int(p["yaw"]), posmod(int(p["tilt"]) + 1, 3))):
		_say(&"editor_shell.rotate.blocked")


## The one selected part as a PlacedPartDict, or an empty Dictionary.
func _single_selected() -> Dictionary:
	var e := _editor()
	var parts: Variant = Kit.as_dict(e.call(&"selection")).get("parts", PackedInt32Array())
	if not parts is PackedInt32Array or (parts as PackedInt32Array).size() != 1:
		return {}
	var slot: int = (parts as PackedInt32Array)[0]
	for p: Dictionary in Kit.dicts(e.call(&"parts")):
		if int(p.get("slot", -1)) == slot:
			return p
	return {}


## Esc: stop the drag, then stop placing, then leave the links tool, then clear the selection, then drop
## a Fix or Suggest preview.
func _cancel() -> void:
	var e := _editor()
	if _drag != Drag.NONE:
		_end_drag(true)
	elif _has_ghost():
		e.call(&"ghost_cancel")
	elif _tool() == Bp.TOOL_ROUTE:
		_route_first = -1
		_set_tool(Bp.TOOL_SELECT)
	elif _has_selection():
		e.call(&"clear_selection")
	else:
		e.call(&"preview_clear")
	_clear_message()
	_refresh_hint()


# --- context menu and Why? ------------------------------------------------------------------------

func _open_context(pos: Vector2) -> void:
	var hit := _pick(pos)
	_context.call(&"open_dock", _display.get_global_transform_with_canvas() * pos, hit, _has_ghost())


func _open_why(slot: int) -> void:
	_context.call(&"show_why_dock", _display.get_global_transform_with_canvas() * (_display.size * 0.5), slot)


func _on_why_reply(why: Dictionary) -> void:
	if _in_splash:
		_context.call(&"show_why_reply", why)


## A Why? link: open the graph on the node it names, or select and frame the part.
func _on_why_link(node: int, part: int) -> void:
	if node >= 0 and _mode_buttons[Mode.GRAPH].visible and not _in_splash:
		_set_mode(Mode.GRAPH)
		if _graph.has_method(&"focus_node"):
			_graph.call(&"focus_node", node)
		return
	if part >= 0:
		Kit.call_on(&"Editor", &"select_parts", [PackedInt32Array([part]), false])
		_frame_view(&"selection")


# --- splash ---------------------------------------------------------------------------------------

func _open_splash_options() -> void:
	if not _allowed(String(_splash_more.get_meta(&"control_id", "editor.splash"))):
		return
	var r := _splash_more.get_global_rect()
	_splash_options.reset_size()
	var size := Vector2(_splash_options.size).max(Vector2(420.0, 200.0))
	var at := Vector2(r.end.x - size.x, r.end.y)
	if not get_viewport().gui_embed_subwindows:
		at = get_viewport().get_screen_transform() * at + Vector2(get_window().position)
	_splash_options.popup(Rect2i(Vector2i(at), Vector2i(size)))


func _do_splash() -> void:
	var e := _editor()
	if e == null or not _is_open() or _in_splash:
		return
	if not _allowed(String(_splash.get_meta(&"control_id", "editor.splash"))):
		return
	_end_drag(false)
	_commit_name()
	# Splash saves first (L216); that save is part of the splash, not a "Design saved" moment.
	_quiet_save = true
	var ok := bool(e.call(&"splash", Kit.as_dict(e.call(&"splash_options"))))
	_quiet_save = false
	if not ok:
		_say(_refusal(&"editor_shell.splash.failed"))
		return
	_enter_splash()


func _enter_splash() -> void:
	_in_splash = true
	_clear_message()
	_context.call(&"close")
	_splash_options.hide()
	_frame.visible = false
	_viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED
	Screens.show_caption(&"editor_shell.splash.loading")
	var world_scene := load(WORLD_SCENE) as PackedScene
	if world_scene != null:
		_world = world_scene.instantiate() as Node3D
		add_child(_world)
	var hud_scene := load(HUD_SCENE) as PackedScene
	if hud_scene != null:
		_hud = hud_scene.instantiate()
		add_child(_hud)
	_splash_ui.visible = true
	_on_splash_band(float(_splash_bar.call(&"band_height")))
	_tune.set_pressed_no_signal(false)
	_tune.tooltip_text = tr(&"editor_shell.splash.tune_tip")
	_tune_dock.visible = false
	_refresh_launch()


func _on_session_state(state: int) -> void:
	if not _in_splash:
		return
	if state == Bp.SESSION_IN_SORTIE:
		Screens.hide_loading()
	elif (state == Bp.SESSION_FAILED or state == Bp.SESSION_IDLE) and not bool(Kit.call_on(&"Editor", &"in_splash")):
		_leave_splash_world()


func _on_splash_ended(_card: Dictionary) -> void:
	var session := Kit.autoload(&"Session")
	if session != null and int(session.call(&"state")) != Bp.SESSION_IDLE:
		session.call(&"leave", false)
	if _in_splash:
		_leave_splash_world()
	# The report card (ui-editor-splash) shows itself from the same signal.


func _on_returned_to_dock(text: Dictionary) -> void:
	_say(text)


func _leave_splash_world() -> void:
	Screens.hide_loading()
	_put_tabs_back()
	if is_instance_valid(_hud):
		_hud.queue_free()
	if is_instance_valid(_world):
		_world.queue_free()
	_hud = null
	_world = null
	_in_splash = false
	_splash_ui.visible = false
	_frame.visible = true
	_set_mode(_mode)
	_refresh_all()


## The splash bar's band: the basin's HUD keeps its docks below it, and the Tune dock opens under it. The
## bar is also a host occluder for the HUD's track labels.
func _on_splash_band(height: float) -> void:
	_tune_dock.offset_top = height
	if is_instance_valid(_hud) and _hud.has_method(&"set_top_inset"):
		_hud.call(&"set_top_inset", height)
		var bar_panel := _splash_bar.get_node_or_null(^"Panel") as Control
		var readout := _splash_bar.get_node_or_null(^"Readout") as Control
		var host: Array[Control] = []
		for c: Control in [bar_panel, readout]:
			if c != null:
				host.append(c)
		_hud.call(&"set_host_occluders", &"splash_bar", host)


## Live edits during a splash (L216): the build tabs move into the Tune dock over the basin.
func _on_tune_toggled(on: bool) -> void:
	if on:
		_left_tabs.owner = null
		_left_tabs.reparent(_tune_dock, false)
		_tune_dock.visible = true
	else:
		_put_tabs_back()


func _put_tabs_back() -> void:
	if _left_tabs.get_parent() != _left_dock:
		_left_tabs.reparent(_left_dock, false)
		_left_tabs.owner = self
	_tune_dock.visible = false
	_tune.set_pressed_no_signal(false)


# --- tutorial -------------------------------------------------------------------------------------

func _allowed(id: String) -> bool:
	var t := Kit.autoload(&"Tutorial")
	if t == null or not t.has_method(&"is_allowed"):
		return true
	return bool(t.call(&"is_allowed", id))


## A step that points at a control brings it on screen: the Hull tab's shape fields and handles for a
## handle, the tray for the tray, Ship's Notes for a note.
func _on_tutorial_step(step: Dictionary) -> void:
	var at := String(step.get("point_at", ""))
	if (at.begins_with("editor.handle.") or at.begins_with("editor.tray")) and _compact:
		# The build dock comes back from behind the reports on a narrow screen.
		_reports.button_pressed = false
	if at.begins_with("editor.handle."):
		_set_mode(Mode.SHIP)
		_left_tabs.current_tab = LEFT_HULL
		_hull.call(&"show_page", 0)
		_set_tool(Bp.TOOL_HULL_HANDLES)
	elif at.begins_with("editor.tray"):
		_left_tabs.current_tab = LEFT_PARTS
	elif at.begins_with("editor.officers"):
		_left_tabs.current_tab = LEFT_OFFICERS
	elif at.begins_with("editor.notes"):
		_right_tabs.current_tab = 0
		if _compact:
			_reports.button_pressed = true
	_apply_tutorial()
	_refresh_hint()


## Disable what the current tutorial step does not allow (no step, or an empty allow list, allows all),
## then the panels do the same for their own controls.
func _apply_tutorial() -> void:
	if not is_node_ready():
		return
	var open := _is_open()
	var undo := _undo_state if open else {}
	var launch := _launch_state if open else {}
	var base: Dictionary[Control, bool] = {
		_undo: Kit.get_bool(undo, "can_undo"), _redo: Kit.get_bool(undo, "can_redo"), _save: open, _file: open,
		_splash: open and not _in_splash, _splash_more: open, _symmetry: open, _slice: open, _camera_menu: open,
		_launch: open and Kit.get_bool(launch, "can_launch") and not _in_splash, _leave: true,
	}
	for c: Control in base:
		var ok: bool = base[c] and _allowed(String(c.get_meta(&"control_id", "")))
		(c as BaseButton).disabled = not ok
	_name.editable = open and _allowed("editor.name")
	# The Launch button draws its own content (text, Caution triangle, count); disabled, it falls back to
	# the button's own text so the theme's disabled colour applies.
	_launch_content.visible = not _launch.disabled
	_launch.text = _launch_text.text if _launch.disabled else ""
	_overlays.visible = open and _mode == Mode.SHIP
	for panel: Control in [_tray, _inspector, _hull, _paint]:
		if panel.has_method(&"apply_tutorial"):
			panel.call(&"apply_tutorial")


## The canvas rectangle of a control by tutorial id, for the overlay's pointer. A hull handle id
## (`editor.handle.beam`) points at its 3D gizmo on the station being edited, else the first one shown;
## panel controls are found by their `control_id` metadata. An empty Rect2 when nothing matches.
## `station` (optional) asks for a handle's gizmo on that station first (the tutorial's beam demonstration).
func control_rect(id: String, station: int = -1) -> Rect2:
	if id.begins_with("editor.handle."):
		var handle := id.trim_prefix("editor.handle.")
		var focus_station := station if station >= 0 else int(_hull.call(&"current_station"))
		var found_rect := Rect2()
		for h: Dictionary in _handles:
			if String(h.get("id", "")) != handle:
				continue
			var at := _gizmos.get_global_transform_with_canvas() * Vector2(h.get("screen", Vector2.ZERO))
			var r := Rect2(at - Vector2(16.0, 16.0), Vector2(32.0, 32.0))
			if int(h.get("station", -1)) == focus_station:
				return r
			if found_rect.size == Vector2.ZERO:
				found_rect = r
		if found_rect.size != Vector2.ZERO:
			return found_rect
	var found := _find_control(self, id)
	return found.get_global_rect() if found != null else Rect2()


func _find_control(node: Node, id: String) -> Control:
	if node is Control and (node as Control).is_visible_in_tree() and node.has_meta(&"control_id") and String(node.get_meta(&"control_id")) == id:
		return node as Control
	for child: Node in node.get_children():
		var f := _find_control(child, id)
		if f != null:
			return f
	return null
