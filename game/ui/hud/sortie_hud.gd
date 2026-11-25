extends CanvasLayer
## Sortie HUD root: docks, view switch, input routing, overlays.
## DESIGN.md L104-116, L1653-1674. Unit: ui-hud-shell.
## Reads and calls: Session, Orders, Hud, BpWorldView.toggle_view, Settings.get_value("hud.swap_docks").
##
## Layout (base 1920 x 1080 canvas units; every 16:9 window has that canvas, and the UI scale
## divides it, so 200% is 960 x 540). The bridge view is the sea and the ship: the 3D view and the
## track symbols fill the whole frame, and the clusters sit flush against the screen edges with
## open sea between them (rule 14: sparse by default; detail is one click away in the stations).
## - Left dock (L106): the track list at the top, the track card under it while a track is
##   selected, and the weapons groups at the bottom (what the guns are doing sits by the targets).
## - Right dock (L106): the time controls and the Bridge Board at the top, the special-operations
##   bar under them, the objectives and the station buttons (log, damage control, networks,
##   control layers) at the bottom; a station opens in the centre (rule 17: every station is three
##   actions from the plot). `hud.swap_docks` swaps the two edges.
## - Centre: the alert strip, the role strip and the status box at the top; officer lines and the
##   helm cluster at the bottom; an open station fills the space between them.
## Each dock's clusters share one opaque backing per run of adjacent clusters, divided by 1 px
## rules; the empty space between runs passes clicks to the sea. In the plot view the plot fills
## the centre only and both docks back their full height, so the chart reads as one table. Each
## dock caps its tallest clusters (the track list, the Bridge Board) so the column never runs off
## the screen; they scroll inside the cap (floor 2). Where a dock's fixed clusters alone pass its
## height (a large UI scale), the dock itself scrolls on one axis. In the centre the alert strip
## shows as many rows as fit over the helm cluster (`set_max_rows`), the rest counting into "+N".
## The bridge overlay keeps track labels and off-view markers clear of every cluster
## (`set_occluders`).
## Clusters over the 3D view get the HUD backing opacity (`access.hud_backing_opacity`, rule 18),
## forced opaque under reduce transparency; the docks themselves are opaque.
##
## Input: every sortie action is read in `_unhandled_input` (L1738), so a focused control never
## sends an order, and Tab, Space and the arrow keys never move focus during play. Keyboard, mouse,
## trackpad and gamepad all reach the same orders (L108):
## - Tab or Back: bridge view and plot table. Esc closes an open station, else opens the pause menu;
##   Start always opens the pause menu. P: pause (single-player).
## - = and -, D-pad left and right: time rate. With `input.compression_hold` in co-op the faster key
##   requests the higher rate only while it is held (L114).
## - W and S, left stick forward and back: the engine telegraph. A key press steps it a quarter, the
##   stick runs it; it stays where it is set and takes the helm, starting from the officer's ordered
##   speed. A and D, left stick sideways: the rudder, centred on release (`Orders.set_helm_input`).
##   The helm's key, or Hand back, gives the helm back and the telegraph to the officer.
## - H, 1 to 4, 5, F or left trigger: role keys through `Orders.role_key`, on press and release,
##   so a hold takes the role and a press under 250 ms toggles it (L108, setting `input.holds_toggle`).
##   [ and ], shoulder buttons: which weapons group F takes.
## - Left click a track: designate it (both views, L108); left drag on the sea, middle drag, right
##   stick, arrow keys, a two-finger drag: orbit; wheel, Page Up and Down, D-pad up and down, pinch:
##   zoom. Right click the sea: set course (Shift appends a waypoint). A on a gamepad designates
##   the selected track; X sets course along the camera's bearing. On the plot the plot table and
##   its cursor take these instead.
## - While a weapons group or mount is held, left click or right trigger fires and does not
##   designate: a click sends one aimed shot (`Orders.fire_at_reticle`), a pull held past 180 ms holds
##   the trigger (`Orders.set_trigger`). On a gamepad the right stick then moves the reticle and turns
##   the camera only past the side of the view.
## - . and , or Y: next and previous track. Backspace or B: leave a plot tool, then clear the
##   selection; with nothing left to cancel, B moves gamepad focus into the docked panels and back.
## - Z, right stick press: acknowledge. X: silence. E: next emission state. C: fire every decoy
##   launcher. L: the log.
## Holds never stick: when a screen or modal opens over the HUD, the window loses focus or the
## session changes state, every role key that is down is released, the helm axes return to zero and
## the trigger lets go.
##
## Splash gate (L216, L245, implementation notes item 9): while the session is a Splash and the dry
## dock is open (`Editor.is_open()`), the dock owns the mouse and every key that also triggers an
## `editor_*` action (R, X, F, Z, Esc, Backspace and the rest); this HUD then acts only on gamepad
## events and keys no dry-dock action uses. `splash` and `splash_rewind` belong to the dock and
## are never handled here. `shares_input_with_dock(event)` answers the question for the dry dock.
##
## Other scripts may call: `set_view(view)`, `view()`, `centre_on_log(entry)` (switches to the plot
## and centres on a `LogEntryDict`), `open_station(name)`, `current_group()`, `world_view()`,
## `plot_view()`, `clusters()` and `set_top_inset(px)` (a host band across the top, such as the splash
## bar, that the docks keep below). Signals: `view_switched(view)`, `current_group_changed(group)`.

signal view_switched(view: int)
signal current_group_changed(group: int)

const Kit := preload("res://ui/screens/menu_kit.gd")
const _Text := preload("res://ui/widgets/ui_text.gd")

## A course order to a ship ordered below this speed also orders COURSE_DEFAULT_SHARE of top speed.
const COURSE_MIN_KN := 0.5
const COURSE_DEFAULT_SHARE := 0.5
## A fire pull at least this long holds the trigger; a shorter one is a single aimed shot.
const FIRE_HOLD_MS := 180
## A press that moves less than this is a click and fires on release (accessibility floor 5).
const CLICK_SLOP_PX := 5.0
## Mouse orbit, degrees per canvas pixel at speed 1.0.
const ORBIT_DEG_PER_PX := 0.25
## Two-finger trackpad drag, degrees per unit of pan delta.
const PAN_DEG_PER_UNIT := 2.0
## Stick and arrow-key orbit at full deflection, degrees per second at speed 1.0.
const STICK_ORBIT_DEG_S := 90.0
## Gamepad plot cursor speed at full deflection, canvas pixels per second.
const CURSOR_PX_S := 700.0
## A pinch has to grow or shrink by this factor before it counts as one zoom step.
const PINCH_STEP := 1.25
## Gamepad set course: the waypoint goes this far along the camera's bearing.
const PAD_COURSE_M := 5000.0
## Analogue triggers count as pressed above this and released below the lower mark.
const TRIGGER_ON := 0.5
const TRIGGER_OFF := 0.3
## Docks take this share of the visible width, within the limits below (canvas units).
const DOCK_SHARE := 0.245
const DOCK_MIN := 280.0
const DOCK_MAX := 470.0
## Docks at least this wide show the station buttons two to a row; narrower, one to a row.
const STATIONS_TWO_UP_W := 360.0
## Height shares of a dock the weapons panel and the track card may take before they scroll.
const WEAPONS_SHARE := 0.38
const CARD_SHARE := 0.42
## An open station keeps at least this share of the centre's height; the alert rows give way first.
const STATION_MIN_SHARE := 0.55
## The Bridge Board keeps at least this share of its dock's height.
const BOARD_MIN_SHARE := 0.45
const BOARD_MIN_H := 160.0
## A dock's fill under this height joins the panels either side of it into one backing.
const FILL_SEAM_MAX := 24.0
## Floors the weapons panel and the track card keep when the left dock overflows: the title with
## one group, the card's header with its buttons and stance.
const WEAPONS_MIN_H := 170.0
const CARD_MIN_H := 200.0
## The track list keeps at least this much height for its header and two rows.
const TRACKS_MIN_H := 150.0
## The helm cluster's width at the bottom of the centre: speed and heading side by side.
const HELM_W := 660.0

## Engine telegraph under manual helm: W and S (or a pad button bound to them) move it one step per
## press, the stick's forward axis moves it at this rate per second at full deflection. It stays
## where it was set, so the ship keeps its speed with the keys up; the rudder (A and D, the stick's
## side axis) is sprung and centres on release.
const TELEGRAPH_STEP := 0.25
const TELEGRAPH_STICK_RATE := 0.8

const AXES: Array[StringName] = [
	&"helm_ahead", &"helm_astern", &"helm_port", &"helm_starboard",
	&"look_left", &"look_right", &"look_up", &"look_down",
]
const STATIONS: Array[StringName] = [&"log", &"damage", &"networks", &"control"]
const STATION_TITLES: Dictionary[StringName, StringName] = {
	&"log": &"hud_shell.station_title.log",
	&"damage": &"hud_shell.station_title.damage",
	&"networks": &"hud_shell.station_title.networks",
	&"control": &"hud_shell.station_title.control",
}
## Station button labels and the icons they shrink to in a narrow dock (floor 2: abbreviation
## allowed at 200%; the name stays in the tooltip and the accessible name).
const STATION_LABELS: Dictionary[StringName, StringName] = {
	&"log": &"hud_shell.station.log",
	&"damage": &"hud_shell.station.damage",
	&"networks": &"hud_shell.station.networks",
	&"control": &"hud_shell.station.control",
}
const STATION_ICONS: Dictionary[StringName, StringName] = {
	&"log": &"view_list",
	&"damage": &"part_damage_control",
	&"networks": &"part_power",
	&"control": &"part_control",
}
## Actions that are never this HUD's (the dry dock and the splash bar own them).
const DOCK_ONLY: Array[StringName] = [&"splash", &"splash_rewind"]
const UI_NAV: Array[StringName] = [&"ui_up", &"ui_down", &"ui_left", &"ui_right"]

## Controls of the host drawn over the HUD (`set_host_occluders`).
var _host_occluders: Dictionary[StringName, Array] = {}
@onready var _frame: Control = $Frame
@onready var _view_root: Control = $Frame/View
@onready var _docks: HBoxContainer = $Frame/Docks
@onready var _left: ScrollContainer = $Frame/Docks/LeftDock
@onready var _right: ScrollContainer = $Frame/Docks/RightDock
@onready var _centre: Control = $Frame/Docks/Centre
@onready var _left_column: VBoxContainer = $Frame/Docks/LeftDock/Column
@onready var _right_column: VBoxContainer = $Frame/Docks/RightDock/Column
@onready var _track_list: Control = $Frame/Docks/LeftDock/Column/TrackList
@onready var _card_rule: HSeparator = $Frame/Docks/LeftDock/Column/CardRule
@onready var _track_card: Control = $Frame/Docks/LeftDock/Column/TrackCard
@onready var _objectives: Control = $Frame/Docks/RightDock/Column/Objectives
@onready var _overlay: Control = $Frame/View/BridgeOverlay
@onready var _reticle: Control = $Frame/View/Reticle
@onready var _plot_table: Control = $Frame/View/PlotTable
@onready var _centre_margin: MarginContainer = $Frame/Docks/Centre/Margin
@onready var _centre_column: VBoxContainer = $Frame/Docks/Centre/Margin/Column
@onready var _top: HBoxContainer = $Frame/Docks/Centre/Margin/Column/Top
@onready var _top_middle: VBoxContainer = $Frame/Docks/Centre/Margin/Column/Top/Middle
@onready var _alert_strip: Control = $Frame/Docks/Centre/Margin/Column/Top/Middle/AlertStrip
## A flow row: two or three held roles wrap under each other instead of running off the centre.
@onready var _role_strip: HFlowContainer = $Frame/Docks/Centre/Margin/Column/Top/Middle/RoleStrip
@onready var _status: PanelContainer = $Frame/Docks/Centre/Margin/Column/Top/Middle/Status
@onready var _status_margin: MarginContainer = $Frame/Docks/Centre/Margin/Column/Top/Middle/Status/Margin
@onready var _status_title: Label = $Frame/Docks/Centre/Margin/Column/Top/Middle/Status/Margin/Column/Title
@onready var _status_body: Label = $Frame/Docks/Centre/Margin/Column/Top/Middle/Status/Margin/Column/Body
@onready var _status_progress: ProgressBar = $Frame/Docks/Centre/Margin/Column/Top/Middle/Status/Margin/Column/Progress
@onready var _middle: HBoxContainer = $Frame/Docks/Centre/Margin/Column/Middle
@onready var _station: Control = $Frame/Docks/Centre/Margin/Column/Middle/Station
@onready var _station_margin: MarginContainer = $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin
@onready var _station_stack: VBoxContainer = $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack
@onready var _station_title: Label = $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Header/Title
@onready var _station_close: Button = $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Header/Close
@onready var _bottom: VBoxContainer = $Frame/Docks/Centre/Margin/Column/Bottom
@onready var _officer_lines: Control = $Frame/Docks/Centre/Margin/Column/Bottom/OfficerLines
@onready var _bottom_row: HBoxContainer = $Frame/Docks/Centre/Margin/Column/Bottom/Row
@onready var _helm: Control = $Frame/Docks/Centre/Margin/Column/Bottom/Row/HelmCluster
@onready var _pacing: Control = $Frame/Docks/RightDock/Column/PacingBar
@onready var _board_rule: HSeparator = $Frame/Docks/RightDock/Column/BoardRule
@onready var _actions_rule: HSeparator = $Frame/Docks/RightDock/Column/ActionsRule
@onready var _actions_scroll: ScrollContainer = $Frame/Docks/RightDock/Column/ActionsScroll
@onready var _actions_bar: Control = $Frame/Docks/RightDock/Column/ActionsScroll/ActionsBar
@onready var _weapons: Control = $Frame/Docks/LeftDock/Column/WeaponsPanel
@onready var _stations_rule: HSeparator = $Frame/Docks/RightDock/Column/StationsRule
@onready var _stations_margin: MarginContainer = $Frame/Docks/RightDock/Column/Stations
@onready var _stations_grid: GridContainer = $Frame/Docks/RightDock/Column/Stations/Grid
@onready var _kill_cam: Control = $Frame/Overlays/KillCam
@onready var _bridge_board: Control = $Frame/Docks/RightDock/Column/BridgeBoard
@onready var _why_popup: Control = $Frame/Overlays/WhyPopup
@onready var _focus_sink: Control = $FocusSink

var _world_ref: WeakRef = null
var _world_searched_ms := -1000
var _view := 0
var _live := false
var _state := 0
var _kind := -1
var _net := 0
var _paused := false
## Action strength per axis action, from the events this HUD saw (keys 0 or 1, sticks 0..1).
var _strength: Dictionary[StringName, float] = {}
var _helm_sent := Vector2.ZERO
## Engine telegraph, -1 (full astern) to 1 (full ahead); sent as the throttle while the player holds
## the helm, zero while the officer has it.
var _telegraph := 0.0
var _helm_mine := false
## The officer's last ordered speed as a telegraph setting (see `_on_motion`).
var _officer_telegraph := 0.0
## When the telegraph took the helm: the board may still show the officer for a moment after.
var _helm_seized_ms := -100000
## The open centre view last given to the reticle for gamepad aiming (canvas coordinates).
var _aim_bounds := Rect2()
## When the fire button went down while a weapon role was held, or -1 (see `_fire_edge`).
var _fire_down_ms := -1
## [kind, index] of the weapon role the player holds, or [] (the role a click fires).
var _weapon_role: Array = []
## Role actions whose press went to `Orders.role_key` and whose release has not: action to [kind, index].
var _roles_down: Dictionary[StringName, Array] = {}
var _lt_down := false
var _rt_down := false
var _trigger := false
var _weapon_held := false
## Mouse press being tracked: { "action": StringName, "pos": Vector2, "moved": bool }.
var _press: Dictionary = {}
var _orbit_drag := false
var _pinch := 1.0
var _groups: Array[int] = []
var _current_group := 0
var _group_note_until_ms := 0
## Role strip: key "kind:index" to { "until_ms": int } for roles that changed hands recently.
var _changed_roles: Dictionary[String, int] = {}
var _strip_expiry_ms := 0
var _strip_shown: Array = []
var _station_name: StringName = &""
var _station_nodes: Dictionary[StringName, Control] = {}
var _station_buttons: Dictionary[StringName, Button] = {}
var _editor_actions: Array[StringName] = []
var _compression_hold_active := false
var _hud_focus_by_pad := false
## A control that took focus from a mouse press; it lets go once the buttons are up.
var _focus_to_release: WeakRef = null
## Widest a role strip entry's words may run: the centre less the glyph, key and margins.
var _strip_text_w := 400.0
## A dock refit is queued for the end of the frame.
var _fit_queued := false


func _ready() -> void:
	_frame.mouse_filter = Control.MOUSE_FILTER_IGNORE
	# The docks meet the centre with no gap: the centre's own margin keeps its clusters off them.
	_docks.add_theme_constant_override(&"separation", 0)
	Ui.set_separation(_left_column, 0)
	Ui.set_separation(_right_column, 0)
	Ui.set_separation(_centre_column, 2)
	Ui.set_separation(_top, 3)
	Ui.set_separation(_top_middle, 1)
	_role_strip.add_theme_constant_override(&"h_separation", Ui.spacing(2))
	_role_strip.add_theme_constant_override(&"v_separation", Ui.spacing(1))
	Ui.set_separation(_middle, 3)
	Ui.set_separation(_bottom, 2)
	Ui.set_separation(_bottom_row, 3)
	Ui.set_separation(_station_stack, 2)
	Ui.set_separation(_stations_grid, 1)
	# The centre's clusters keep 16 px from the docks and the screen edges.
	Ui.set_margins(_centre_margin, 3)
	Ui.set_margins(_status_margin, 3)
	Ui.set_margins(_station_margin, 3)
	Ui.set_margins(_stations_margin, 3)
	# Each dock draws one opaque backing under each run of adjacent clusters (no card per cluster).
	_left_column.draw.connect(_draw_dock_backing.bind(_left_column))
	_right_column.draw.connect(_draw_dock_backing.bind(_right_column))
	for column: VBoxContainer in [_left_column, _right_column]:
		column.sort_children.connect(column.queue_redraw)
		for c: Node in column.get_children():
			if c is Control:
				(c as Control).visibility_changed.connect(_on_dock_child_changed)
				(c as Control).minimum_size_changed.connect(_on_dock_child_changed)
	_status_title.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_status_body.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_station_title.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_left.accessibility_name = tr(&"hud_shell.access.left_dock")
	_right.accessibility_name = tr(&"hud_shell.access.right_dock")
	_station.accessibility_name = tr(&"hud_shell.access.station")
	_station_close.icon = Ui.icon(&"close")
	_station_close.accessibility_name = tr(&"hud_shell.station.close")
	_station_close.tooltip_text = tr(&"hud_shell.station.close")
	_station_close.pressed.connect(close_station)
	_station_nodes = {
		&"log": $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Log,
		&"damage": $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Damage,
		&"networks": $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Networks,
		&"control": $Frame/Docks/Centre/Margin/Column/Middle/Station/Margin/Stack/Body/Panels/Control,
	}
	_station_buttons = {
		&"log": $Frame/Docks/RightDock/Column/Stations/Grid/Log,
		&"damage": $Frame/Docks/RightDock/Column/Stations/Grid/Damage,
		&"networks": $Frame/Docks/RightDock/Column/Stations/Grid/Networks,
		&"control": $Frame/Docks/RightDock/Column/Stations/Grid/Control,
	}
	for name: StringName in STATIONS:
		var b := _station_buttons[name]
		b.focus_mode = Control.FOCUS_ALL
		b.pressed.connect(_on_station_button.bind(name))
	Kit.chain_horizontal([_station_buttons[&"log"], _station_buttons[&"damage"]])
	Kit.chain_horizontal([_station_buttons[&"networks"], _station_buttons[&"control"]])
	for action: StringName in InputMap.get_actions():
		if String(action).begins_with("editor_"):
			_editor_actions.append(action)
	for a: StringName in AXES:
		_strength[a] = 0.0
	_connect_facades()
	_wire_children()
	get_viewport().size_changed.connect(_layout)
	_centre.resized.connect(_place_view)
	for d: Control in [_left, _right]:
		d.minimum_size_changed.connect(_layout, CONNECT_DEFERRED)
	_centre.resized.connect(_fit_alerts)
	for c: Control in [_alert_strip, _bottom, _status, _role_strip]:
		c.minimum_size_changed.connect(_fit_alerts, CONNECT_DEFERRED)
		c.visibility_changed.connect(_fit_alerts, CONNECT_DEFERRED)
	get_viewport().gui_focus_changed.connect(_on_focus_changed)
	if _overlay.has_method(&"set_occluders"):
		_overlay.call(&"set_occluders", clusters())
	if _overlay.has_method(&"set_edge_area"):
		_overlay.call(&"set_edge_area", _centre)
	Ui.changed.connect(_on_ui_changed)
	_apply_swap()
	_apply_backing()
	_layout.call_deferred()
	_refresh_session()
	_refresh_groups()
	_refresh_held()
	_refresh_strip()
	_apply_view(_world_view_value())


func _exit_tree() -> void:
	_release_all_input()


## Screens calls this after `_ready`; the HUD needs no arguments.
func setup(_args: Dictionary) -> void:
	_refresh_session()


## The HUD takes no focus when it opens: keys belong to the sortie until the player moves into a
## panel on purpose (Shift+Tab, or B on a gamepad).
func default_focus() -> Control:
	return _focus_sink


func _notification(what: int) -> void:
	match what:
		NOTIFICATION_APPLICATION_FOCUS_OUT, NOTIFICATION_WM_WINDOW_FOCUS_OUT:
			_release_all_input()
		NOTIFICATION_TRANSLATION_CHANGED:
			if is_node_ready():
				_refresh_status()
				_refresh_strip()
				if _station_name != &"":
					_station_title.text = tr(STATION_TITLES[_station_name])


# --- wiring --------------------------------------------------------------------------------------

func _connect_facades() -> void:
	_on(&"Session", &"state_changed", _on_session_state)
	_on(&"Session", &"pacing_changed", _on_pacing)
	_on(&"Session", &"loading_progress", _on_loading)
	_on(&"Session", &"sortie_started", _refresh_status)
	_on(&"Hud", &"board_changed", _refresh_held)
	_on(&"Hud", &"motion_changed", _on_motion)
	_on(&"Hud", &"control_changed", _on_control_changed)
	_on(&"Hud", &"mounts_changed", _refresh_groups)
	_on(&"Hud", &"ship_static_changed", _on_ship_static)
	_on(&"Hud", &"selection_changed", _on_selection)
	_on(&"Hud", &"motion_changed", _on_first_motion)
	_on(&"Settings", &"changed", _on_setting)
	_on(&"Screens", &"stack_changed", _on_stack)
	_on(&"Screens", &"modal_changed", _on_modal)


func _on(autoload_name: StringName, signal_name: StringName, handler: Callable) -> void:
	var node := Kit.autoload(autoload_name)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _wire_children() -> void:
	if _plot_table.has_method(&"set_actions"):
		_plot_table.call(&"set_actions", _actions_bar)
	var plot := plot_view()
	if plot != null and _actions_bar.has_method(&"set_plot"):
		_actions_bar.call(&"set_plot", plot)
	_actions_bar.minimum_size_changed.connect(_on_actions_size, CONNECT_DEFERRED)
	_actions_bar.visibility_changed.connect(_on_actions_size, CONNECT_DEFERRED)
	if _track_card.has_signal(&"close_requested"):
		_track_card.connect(&"close_requested", func() -> void: Kit.call_on(&"Hud", &"select_track", [Bp.NONE]))
	if _track_card.has_signal(&"centre_requested"):
		_track_card.connect(&"centre_requested", func(t: int) -> void: centre_on_log({"track": t}))
	if _bridge_board.has_signal(&"focus_targets_changed"):
		_bridge_board.connect(&"focus_targets_changed", _link_panels, CONNECT_DEFERRED)
	_on_actions_size()
	_on_selection(int(Kit.call_on(&"Hud", &"selected_track")) if Kit.call_on(&"Hud", &"selected_track") is int else Bp.NONE)


func _on_actions_size() -> void:
	# The bar sizes itself; in a dock narrower than the bar it scrolls sideways (floor 2). The rule
	# above it shows only while it has rows.
	var has_rows := _actions_bar.visible
	_actions_rule.visible = has_rows
	_actions_scroll.visible = has_rows


func _on_selection(track: int) -> void:
	var show := track != Bp.NONE
	_track_card.visible = show
	_card_rule.visible = show
	_link_panels.call_deferred()


## Keyboard and gamepad paths between the panels (L1738: explicit neighbours): from the track
## table, Right goes to the track card when one is open, else to the plot tools in the plot view,
## else to the station buttons; Left from any of those comes back to the table. Inside each panel
## its own chain holds. The Bridge Board sits above the station buttons in the same dock: Up from
## the top row of stations enters it at its last control, Down from that control comes back, and
## Left from any of its controls returns to the table.
func _link_panels() -> void:
	if not is_node_ready():
		return
	var table: Control = null
	if _track_list.has_method(&"default_focus"):
		var v: Variant = _track_list.call(&"default_focus")
		if v is Control:
			table = v
	if table == null:
		return
	var groups: Array = []
	# Left to right as a gamepad player meets them: the left dock (track card, weapons panel with its
	# launch and decoy buttons), the plot's tools, the alert strip (Silence has no pad button, so focus
	# is its only pad route), then the station buttons with the Bridge Board above them.
	for panel: Control in [_track_card, _weapons, _plot_table, _alert_strip, _helm, _objectives]:
		if panel.is_visible_in_tree() and panel.has_method(&"focus_targets"):
			# Only what can take focus now: a link to a hidden control stops the arrow dead.
			var list: Array = (panel.call(&"focus_targets") as Array).filter(func(c: Variant) -> bool:
				return c is Control and is_instance_valid(c) and (c as Control).is_visible_in_tree() and (c as Control).focus_mode != Control.FOCUS_NONE)
			if panel == _alert_strip:
				# One row: Left and Right walk it before Right leads on.
				Kit.chain_horizontal(list)
			if not list.is_empty():
				groups.append(list)
	var stations: Array[Control] = []
	for n: StringName in STATIONS:
		stations.append(_station_buttons[n])
	groups.append(stations)
	var first: Control = groups[0][0]
	table.focus_neighbor_right = table.get_path_to(first)
	for i in groups.size():
		var next_group: Array = groups[i + 1] if i + 1 < groups.size() else []
		for c: Control in groups[i]:
			var own_left := c.get_node_or_null(c.focus_neighbor_left) if not c.focus_neighbor_left.is_empty() else null
			if own_left == null or own_left == c or own_left == table:
				c.focus_neighbor_left = c.get_path_to(table)
			if next_group.is_empty() or c in stations:
				continue
			# A control keeps a right neighbour inside its own panel; the panel's right edge leads on.
			var own_right := c.get_node_or_null(c.focus_neighbor_right) if not c.focus_neighbor_right.is_empty() else null
			if own_right == null or own_right == c or not (own_right in groups[i]):
				c.focus_neighbor_right = c.get_path_to(next_group[0])
	# The station grid keeps its own left and right between its two columns.
	Kit.chain_horizontal([_station_buttons[&"log"], _station_buttons[&"damage"]])
	Kit.chain_horizontal([_station_buttons[&"networks"], _station_buttons[&"control"]])
	_station_buttons[&"log"].focus_neighbor_left = _station_buttons[&"log"].get_path_to(table)
	_station_buttons[&"networks"].focus_neighbor_left = _station_buttons[&"networks"].get_path_to(table)
	Kit.chain_vertical([_station_buttons[&"log"], _station_buttons[&"networks"]])
	Kit.chain_vertical([_station_buttons[&"damage"], _station_buttons[&"control"]])
	_link_board(table)


## The Bridge Board between the table (Left) and the station buttons below it (Up and Down).
func _link_board(table: Control) -> void:
	if not _bridge_board.is_visible_in_tree() or not _bridge_board.has_method(&"focus_targets"):
		return
	var board: Array = _bridge_board.call(&"focus_targets")
	if board.is_empty():
		return
	for c: Control in board:
		c.focus_neighbor_left = c.get_path_to(table)
	var last: Control = board[board.size() - 1]
	last.focus_neighbor_bottom = last.get_path_to(_station_buttons[&"log"])
	for n: StringName in [&"log", &"damage"]:
		_station_buttons[n].focus_neighbor_top = _station_buttons[n].get_path_to(last)

func _on_ship_static() -> void:
	_refresh_groups()
	_refresh_held()
	_refresh_strip()
	_refresh_status()


func _on_first_motion() -> void:
	# The status box says "waiting for the first picture" until the own hull reports in.
	if _status.visible:
		_refresh_status()


# --- the world view ------------------------------------------------------------------------------

## The sortie's BpWorldView: the one Screens built, else the first in the tree. Null in captures
## without a world, and in a check run.
func world_view() -> Node:
	if _world_ref != null:
		var w: Object = _world_ref.get_ref()
		if w != null and is_instance_valid(w) and (w as Node).is_inside_tree():
			return w as Node
		_world_ref = null
	var found: Node = null
	var screens := Kit.autoload(&"Screens")
	if screens != null and screens.has_method(&"world_view"):
		var v: Variant = screens.call(&"world_view")
		if v is Node:
			found = v
	var now := Time.get_ticks_msec()
	if found == null and is_inside_tree() and now - _world_searched_ms >= 1000:
		_world_searched_ms = now
		var all := get_tree().root.find_children("*", "BpWorldView", true, false)
		if not all.is_empty():
			found = all[0]
	if found != null:
		_world_ref = weakref(found)
		if found.has_signal(&"view_changed") and not found.is_connected(&"view_changed", _apply_view):
			found.connect(&"view_changed", _apply_view)
		if _kill_cam.has_method(&"set_world"):
			_kill_cam.call(&"set_world", found)
		if _overlay.has_method(&"set_world"):
			_overlay.call(&"set_world", found)
	return found


## The plot table's BpPlotView.
func plot_view() -> Control:
	if _plot_table.has_method(&"plot"):
		var p: Variant = _plot_table.call(&"plot")
		if p is Control:
			return p
	return null


func _world_view_value() -> int:
	var w := world_view()
	if w != null and w.has_method(&"view"):
		return int(w.call(&"view"))
	return _view


# --- views ---------------------------------------------------------------------------------------

func view() -> int:
	return _view


## `Bp.VIEW_BRIDGE` or `Bp.VIEW_PLOT`. The world view switches the 3D off under the plot.
func set_view(next: int) -> void:
	if next != Bp.VIEW_BRIDGE and next != Bp.VIEW_PLOT:
		return
	var w := world_view()
	if w != null and w.has_method(&"set_view"):
		w.call(&"set_view", next)
	_apply_view(next)


func toggle_view() -> void:
	set_view(Bp.VIEW_PLOT if _view == Bp.VIEW_BRIDGE else Bp.VIEW_BRIDGE)


func _apply_view(next: int) -> void:
	var changed := next != _view
	_view = next
	var plot := _view == Bp.VIEW_PLOT
	_plot_table.visible = plot
	_overlay.visible = not plot and _station_name == &""
	_reticle.visible = not plot
	_place_view()
	_band_plot_tools.call_deferred()
	_left_column.queue_redraw()
	_right_column.queue_redraw()
	_end_drag()
	if plot and _trigger:
		_set_trigger(false)
	_link_panels.call_deferred()
	if changed:
		view_switched.emit(_view)


## The bridge view's overlay covers the whole frame (the sea shows between the clusters); the plot
## table fills the centre between the docks.
func _place_view() -> void:
	if not is_node_ready():
		return
	if _view == Bp.VIEW_PLOT:
		_view_root.set_anchors_preset(Control.PRESET_TOP_LEFT)
		_view_root.position = _docks.position + _centre.position
		_view_root.size = _centre.size
	else:
		_view_root.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)


## Switch to the plot and centre on what a log entry is about (rule 15).
func centre_on_log(entry: Dictionary) -> void:
	set_view(Bp.VIEW_PLOT)
	if _plot_table.has_method(&"centre_on_entry"):
		_plot_table.call(&"centre_on_entry", entry)


# --- stations ------------------------------------------------------------------------------------

## Open one station panel in the centre (`&"log"`, `&"damage"`, `&"networks"`, `&"control"`), or
## close it when it is already open.
func open_station(name: StringName) -> void:
	if not STATIONS.has(name):
		return
	if _station_name == name:
		close_station()
		return
	_station_name = name
	for n: StringName in STATIONS:
		_station_nodes[n].visible = n == name
		_station_buttons[n].set_pressed_no_signal(n == name)
	_station_title.text = tr(STATION_TITLES[name])
	_station.visible = true
	# The station takes the centre between the docks and under the alerts: the helm cluster steps
	# aside while it is open, so the schematic and its lists are not squeezed against the officer
	# lines. Closing the station brings it back.
	_bottom_row.visible = false
	# The station covers the centre the track symbols are drawn in; they would only peek out round
	# its edges, so they wait until it closes.
	_overlay.visible = false
	_layout()
	if not Ui.using_mouse:
		Ui.focus(Screens.first_focusable(_station_nodes[name]) if Kit.autoload(&"Screens") != null else null)


## The open station (`&"log"`, `&"damage"`, `&"networks"`, `&"control"`), or `&""`.
func station() -> StringName:
	return _station_name


func close_station() -> void:
	if _station_name == &"":
		return
	var had_focus := _owns_focus()
	_station_name = &""
	for n: StringName in STATIONS:
		_station_nodes[n].visible = false
		_station_buttons[n].set_pressed_no_signal(false)
	_station.visible = false
	_bottom_row.visible = true
	_overlay.visible = _view == Bp.VIEW_BRIDGE
	_fit_alerts.call_deferred()
	if had_focus:
		get_viewport().gui_release_focus()


func _on_station_button(name: StringName) -> void:
	open_station(name)


# --- layout --------------------------------------------------------------------------------------

func _layout() -> void:
	if not is_inside_tree():
		return
	var w := _frame.get_rect().size.x
	if w <= 1.0:
		w = get_viewport().get_visible_rect().size.x
	var dock := floorf(clampf(w * DOCK_SHARE, DOCK_MIN, DOCK_MAX))
	_left.custom_minimum_size = Vector2(dock, 0.0)
	_right.custom_minimum_size = Vector2(dock, 0.0)
	# Station buttons sit two to a row with their names while the dock holds them; in a narrow dock
	# (large UI scale) they shrink to their icons, four to a row.
	var narrow := dock < STATIONS_TWO_UP_W
	_stations_grid.columns = 4 if narrow else 2
	# A station in a narrow centre keeps 8 px margins, not 16, so its panel keeps the width.
	Ui.set_margins(_station_margin, 1 if narrow else 3)
	for n: StringName in STATIONS:
		var b := _station_buttons[n]
		b.text = "" if narrow else String(STATION_LABELS[n])
		b.icon = Ui.icon(STATION_ICONS[n]) if narrow else null
		b.icon_alignment = HORIZONTAL_ALIGNMENT_CENTER
		b.tooltip_text = tr(STATION_LABELS[n])
		b.accessibility_name = tr(STATION_LABELS[n])
	# The docks' real widths (a scrolling dock's bar can add to its own), not the share asked for.
	var docks_w := maxf(_left.get_combined_minimum_size().x, dock) + maxf(_right.get_combined_minimum_size().x, dock)
	var centre_w := maxf(w - docks_w - float(Ui.spacing(3) * 2), 0.0)
	_helm.custom_minimum_size = Vector2(minf(centre_w, HELM_W), 0.0)
	if _alert_strip.has_method(&"set_max_width"):
		_alert_strip.call(&"set_max_width", centre_w)
	if _officer_lines.has_method(&"set_max_width"):
		_officer_lines.call(&"set_max_width", centre_w)
	# The entry's glyph, key hint, gaps, margin and panel margin take the rest.
	var strip_w := floorf(centre_w - 24.0 - 40.0 - float(Ui.spacing(1) * 3) - float(Ui.spacing(1) * 2) - float(Ui.spacing(3) * 2))
	if not is_equal_approx(strip_w, _strip_text_w):
		_strip_text_w = strip_w
		_strip_shown = []
		_refresh_strip()
	_fit_docks.call_deferred()
	_place_view.call_deferred()


## Keep the docks and the centre's clusters below a band the host draws across the top of the screen
## (the dry dock's splash bar), so the two never overlap; the 3D view still fills the whole frame. 0
## gives the docks the full height again.
func set_top_inset(px: float) -> void:
	var inset := maxf(roundf(px), 0.0)
	if is_equal_approx(_docks.offset_top, inset):
		return
	_docks.offset_top = inset
	_layout.call_deferred()
	_fit_alerts.call_deferred()


func top_inset() -> float:
	return _docks.offset_top


## Controls a host draws over this HUD (the splash bar, the tutorial card): track labels and off-view
## markers keep clear of them as they do of the clusters. Each host names its own list (`key`), which
## replaces that host's previous one.
func set_host_occluders(key: StringName, controls: Array[Control]) -> void:
	_host_occluders[key] = controls.duplicate()
	if _overlay.has_method(&"set_occluders"):
		var all := clusters()
		for k: StringName in _host_occluders:
			for c: Variant in _host_occluders[k]:
				if c is Control and is_instance_valid(c):
					all.append(c as Control)
		_overlay.call(&"set_occluders", all)


## Share each dock's height out. Right: the time controls, special operations, objectives and
## stations keep their height and the Bridge Board takes the rest, scrolling inside it. Left: the
## weapons panel and the track card up to their shares, the track list the rest; when the three
## still overflow (a large UI scale), the card gives way first, then the weapons panel.
func _fit_docks() -> void:
	if not is_node_ready():
		return
	var h := _right.size.y
	if h <= 1.0:
		h = get_viewport().get_visible_rect().size.y
	var others := _right_column.get_combined_minimum_size().y - _bridge_board.get_combined_minimum_size().y
	# The board takes what the fixed clusters leave; only when they leave it almost nothing (a large
	# UI scale) does it keep a floor, the dock then scrolling so its groups stay reachable.
	_cap(_bridge_board, maxf(floorf(h - others), minf(floorf(h * BOARD_MIN_SHARE), BOARD_MIN_H)))
	var weapons_cap := floorf(h * WEAPONS_SHARE)
	var card_cap := floorf(h * CARD_SHARE)
	_cap(_weapons, weapons_cap)
	_cap(_track_card, card_cap)
	var left_others := _left_column.get_combined_minimum_size().y - _track_list.get_combined_minimum_size().y
	var least := TRACKS_MIN_H
	if _track_list.has_method(&"least_height"):
		least = maxf(float(_track_list.call(&"least_height")), least)
	_cap(_track_list, maxf(floorf(h - left_others), least))
	var over := _left_column.get_combined_minimum_size().y - h
	# Overflow (a large UI scale): the weapons panel gives way to its floor first, then the card to
	# its own; past both floors the dock scrolls.
	if over > 0.5:
		var weapons_h := _weapons.get_combined_minimum_size().y
		_cap(_weapons, maxf(weapons_h - over, minf(weapons_h, WEAPONS_MIN_H)))
		over = _left_column.get_combined_minimum_size().y - h
	if over > 0.5 and _track_card.visible:
		var card_h := _track_card.get_combined_minimum_size().y
		_cap(_track_card, maxf(card_h - over, minf(card_h, CARD_MIN_H)))
	# A dock that still overflows scrolls; one that fits passes every click on its open sea through.
	for pair: Array in [[_left, _left_column], [_right, _right_column]]:
		var dock: ScrollContainer = pair[0]
		var over_h := (pair[1] as Control).get_combined_minimum_size().y > h + 0.5
		dock.mouse_filter = Control.MOUSE_FILTER_PASS if over_h else Control.MOUSE_FILTER_IGNORE
		# The special-operations bar takes the column's width, less the bar of a scrolling dock.
		if dock == _right and _actions_bar.has_method(&"set_panel_width"):
			var bar := dock.get_v_scroll_bar().get_combined_minimum_size().x if over_h else 0.0
			_actions_bar.call(&"set_panel_width", dock.custom_minimum_size.x - bar)
	_fit_alerts()


## The plot's tool column keeps to the band between the alert strip and the helm cluster.
func _band_plot_tools() -> void:
	if not _plot_table.is_visible_in_tree() or not _plot_table.has_method(&"set_tool_band"):
		return
	var to_local := _plot_table.get_global_transform().affine_inverse()
	var top := (to_local * Vector2(0.0, _top.get_global_rect().end.y)).y if _top.visible else 0.0
	var bottom := (to_local * Vector2(0.0, _bottom.get_global_rect().position.y)).y if _bottom.visible and _bottom_row.visible else _plot_table.size.y
	_plot_table.call(&"set_tool_band", maxf(top, 0.0), minf(bottom, _plot_table.size.y))
	# Track and reference labels give way to the centre's clusters rather than sit under them.
	if _plot_table.has_method(&"set_blocked"):
		var rects: Array[Rect2] = []
		for c: Control in [_alert_strip, _role_strip, _status, _station, _officer_lines, _helm]:
			if c.is_visible_in_tree() and c.size.x >= 1.0 and c.size.y >= 1.0:
				rects.append(c.get_global_rect())
		_plot_table.call(&"set_blocked", rects)


## The alert strip shows as many rows as fit between the top of the centre and the helm cluster.
func _fit_alerts() -> void:
	if not _alert_strip.has_method(&"set_max_rows") or not _alert_strip.has_method(&"row_height"):
		return
	var h := _centre.size.y
	if h <= 1.0:
		return
	var margins := float(_centre_margin.get_theme_constant(&"margin_top") + _centre_margin.get_theme_constant(&"margin_bottom"))
	var sep := float(_centre_column.get_theme_constant(&"separation"))
	var below := _bottom.get_combined_minimum_size().y if _bottom.visible else 0.0
	# The helm counts at its own height, not at a cap an earlier crowded moment gave it, so the room
	# comes back to it when the top clears.
	if _bottom.visible and _bottom_row.visible and _helm.has_method(&"natural_height"):
		below += float(_helm.call(&"natural_height")) - _helm.get_combined_minimum_size().y
	if _station.visible:
		# An open station keeps most of the centre; its panel scrolls inside what is left.
		below += maxf(_station_margin.get_combined_minimum_size().y, h * STATION_MIN_SHARE) + sep
	var row := float(_alert_strip.call(&"row_height"))
	var rows_now := 0
	var rows_box := _alert_strip.get_node_or_null(^"Margin/Column/Rows") as Container
	if rows_box != null:
		for r: Node in rows_box.get_children():
			if (r as Control).visible:
				rows_now += 1
	var strip_h := _alert_strip.get_combined_minimum_size().y if _alert_strip.visible else 0.0
	var strip_chrome := strip_h - row * float(rows_now)
	# The role strip and the status box share the top with the alerts.
	var top_others := (_top.get_combined_minimum_size().y - strip_h) if _top.visible else 0.0
	var room := h - margins - sep * 2.0 - below - strip_chrome - top_others
	_alert_strip.call(&"set_max_rows", maxi(1, floori(room / maxf(row, 1.0))))
	_band_plot_tools.call_deferred()
	# With one alert row left and still no room (a large UI scale), the helm's gauges scroll.
	if _helm.has_method(&"set_max_height"):
		var top_h := _top.get_combined_minimum_size().y if _top.visible else 0.0
		var lines_h := _officer_lines.get_combined_minimum_size().y + sep if _officer_lines.visible else 0.0
		_helm.call(&"set_max_height", maxf(h - margins - sep * 2.0 - top_h - lines_h, 0.0))


static func _cap(cluster: Control, height: float) -> void:
	if cluster.has_method(&"set_max_height"):
		cluster.call(&"set_max_height", height)


func _on_dock_child_changed() -> void:
	_left_column.queue_redraw()
	_right_column.queue_redraw()
	if not _fit_queued:
		_fit_queued = true
		(func() -> void:
			_fit_queued = false
			_fit_docks()).call_deferred()


## One opaque backing under each run of adjacent visible clusters in a dock column (the spacer
## breaks a run); in the plot view the whole column is backed. Clusters that are panels of their
## own draw their own backing.
func _draw_dock_backing(column: VBoxContainer) -> void:
	var style := column.get_theme_stylebox(&"panel", &"Panel")
	if style == null:
		return
	var w := column.size.x
	if _view == Bp.VIEW_PLOT:
		column.draw_style_box(style, Rect2(Vector2.ZERO, column.size))
		return
	var top := -1.0
	var bottom := -1.0
	for c: Node in column.get_children():
		var ctl := c as Control
		if ctl == null or not ctl.visible:
			continue
		if ctl.name == &"Fill":
			# A fill squeezed to (nearly) nothing would leave a seam of sea between two panels.
			if ctl.size.y < FILL_SEAM_MAX:
				continue
			if top >= 0.0:
				column.draw_style_box(style, Rect2(0.0, top, w, bottom - top))
			top = -1.0
			continue
		if top < 0.0:
			top = ctl.position.y
		bottom = ctl.position.y + ctl.size.y
	if top >= 0.0:
		column.draw_style_box(style, Rect2(0.0, top, w, bottom - top))


## Every cluster drawn over the view: the bridge overlay keeps its labels and off-view markers clear
## of them, and the layout checks test that no two of them overlap.
func clusters() -> Array[Control]:
	return [_track_list, _card_rule, _track_card, _objectives, _pacing, _bridge_board, _actions_scroll, _weapons, _stations_margin, _alert_strip, _role_strip, _status, _station, _officer_lines, _helm]


func _apply_swap() -> void:
	var swap := _setting_bool(&"hud.swap_docks")
	var first: Control = _right if swap else _left
	var last: Control = _left if swap else _right
	_docks.move_child(first, 0)
	_docks.move_child(_centre, 1)
	_docks.move_child(last, 2)


func _on_setting(key: StringName) -> void:
	match String(key):
		"hud.swap_docks":
			_apply_swap()
		"":
			_apply_swap()


func _on_ui_changed(what: StringName) -> void:
	if what == &"backing" or what == &"transparency":
		_apply_backing()
	elif what == &"scale":
		_layout.call_deferred()


## Panels over the 3D view take the HUD backing opacity on their own background only, so their
## text stays at full contrast (rule 18). Docked panels stay opaque.
func _apply_backing() -> void:
	var a := 1.0 if Ui.reduce_transparency else clampf(Ui.hud_backing_opacity, 0.0, 1.0)
	_status.self_modulate.a = a
	for c: Node in _role_strip.get_children():
		if c is CanvasItem:
			(c as CanvasItem).self_modulate.a = a
	if _overlay.has_method(&"set_backing"):
		_overlay.call(&"set_backing", a)


# --- session state -------------------------------------------------------------------------------

func _refresh_session() -> void:
	var info := Kit.as_dict(Kit.call_on(&"Session", &"info"))
	_state = Kit.get_int(info, "state", 0)
	_kind = Kit.get_int(info, "kind", -1)
	_net = Kit.get_int(info, "net", 0)
	_live = _state == Bp.SESSION_IN_SORTIE or _state == Bp.SESSION_REPLAY or _state == Bp.SESSION_ENDED
	_paused = bool(Kit.as_dict(Kit.call_on(&"Session", &"pacing")).get("paused", false))
	_refresh_status()


func _on_session_state(_state_value: int) -> void:
	_release_all_input()
	_refresh_session()
	if _state == Bp.SESSION_IDLE or _state == Bp.SESSION_LOADING:
		_changed_roles.clear()
		_refresh_strip()
		close_station()


func _on_pacing() -> void:
	_paused = bool(Kit.as_dict(Kit.call_on(&"Session", &"pacing")).get("paused", false))
	_refresh_status()


func _on_loading(fraction: float) -> void:
	if _state == Bp.SESSION_LOADING:
		_status_progress.value = clampf(fraction, 0.0, 1.0)
	_refresh_status()


func _covered() -> bool:
	var screens := Kit.autoload(&"Screens")
	return screens != null and screens.has_method(&"is_loading") and bool(screens.call(&"is_loading"))


## The status box: loading (when no loading cover is up, as in a Splash), waiting for the first
## picture, or paused. Nothing in the normal state (rule 1: quiet when normal).
func _refresh_status() -> void:
	var title := ""
	var body := ""
	var progress := -1.0
	if _state == Bp.SESSION_LOADING:
		if not _covered():
			title = tr(&"hud_shell.status.loading")
			progress = float(Kit.as_dict(Kit.call_on(&"Session", &"info")).get("loading", 0.0))
	elif _state == Bp.SESSION_IN_SORTIE and _waiting_for_picture():
		title = tr(&"hud_shell.status.waiting")
	elif _paused and (_state == Bp.SESSION_IN_SORTIE):
		title = tr(&"hud_shell.status.paused")
		body = tr(&"hud_shell.status.paused_body").format([_binding_text(&"pause")])
	_status.visible = not title.is_empty()
	_status_title.text = title
	_status_body.text = body
	_status_body.visible = not body.is_empty()
	_status_progress.visible = progress >= 0.0
	if progress >= 0.0:
		_status_progress.value = clampf(progress, 0.0, 1.0)


## True in a sortie whose own hull has not reported yet (no motion section ever bumped).
func _waiting_for_picture() -> bool:
	if _kind == Bp.KIND_REPLAY or Kit.call_on(&"Hud", &"has_ship") == true:
		return false
	var rev: Variant = Kit.call_on(&"Hud", &"rev", [&"motion"])
	return rev is int and int(rev) == 0


func _binding_text(action: StringName) -> String:
	var v: Variant = Kit.call_on(&"Settings", &"binding_text", [action])
	if (v is String or v is StringName) and not String(v).is_empty():
		return String(v)
	for e: InputEvent in InputMap.action_get_events(action):
		if e is InputEventKey:
			return (e as InputEventKey).as_text_physical_keycode()
	return tr(&"hud_shell.unbound")


# --- roles ---------------------------------------------------------------------------------------

func _refresh_groups() -> void:
	var out: Array[int] = []
	for g: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"weapon_groups")):
		var n := Kit.get_int(g, "group", -1)
		if n >= 0 and not out.has(n):
			out.append(n)
	out.sort()
	_groups = out
	if not _groups.has(_current_group):
		var before := _current_group
		_current_group = _groups[0] if not _groups.is_empty() else 0
		if before != _current_group:
			current_group_changed.emit(_current_group)
	_refresh_strip()


## The weapons group F (or the left trigger) takes, 0-based.
func current_group() -> int:
	return _current_group


func _step_group(step: int) -> void:
	if _groups.size() < 2:
		return
	var i := _groups.find(_current_group)
	i = (i + step + _groups.size()) % _groups.size() if i >= 0 else 0
	_current_group = _groups[i]
	_group_note_until_ms = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0)
	current_group_changed.emit(_current_group)
	_refresh_strip()


func _refresh_held() -> void:
	var board := Kit.as_dict(Kit.call_on(&"Hud", &"board"))
	var held := false
	var helm := false
	var role: Array = []
	for r: Dictionary in Kit.dicts(board.get("held", [])):
		var k := Kit.get_int(r, "kind", -1)
		if k == Bp.ROLE_MOUNT or k == Bp.ROLE_WEAPON_GROUP:
			held = true
			if role.is_empty():
				role = [k, Kit.get_int(r, "index", 0)]
		elif k == Bp.ROLE_HELM:
			helm = true
	_weapon_held = held
	_weapon_role = role
	if not helm and Time.get_ticks_msec() - _helm_seized_ms < 1500:
		helm = _helm_mine
	if _helm_mine and not helm:
		# The officer has the helm again: the telegraph starts from his order next time.
		_telegraph = 0.0
	elif helm and not _helm_mine:
		# Taken by its key or a button while under way: hold the officer's speed, not ring down.
		# The snapshot that shows the helm held already carries the player's zero throttle, so the
		# speed the officer last ordered is used.
		_telegraph = _officer_telegraph
	_helm_mine = helm
	if not held:
		_fire_down_ms = -1
		if _trigger:
			_set_trigger(false)
	_refresh_strip()


func _on_control_changed(role: Dictionary, _holder: int) -> void:
	var key := "%d:%d" % [Kit.get_int(role, "kind", -1), Kit.get_int(role, "index", 0)]
	_changed_roles[key] = Time.get_ticks_msec() + int(Ui.mode_box_s() * 1000.0)
	_refresh_held()


## The role strip: each role the player holds (with the hand glyph), and for 10 s each role that
## changed hands, inside the mode-change box (rule 9, L1649). Empty and hidden when the officers
## have every role and nothing changed (rule 1).
func _refresh_strip() -> void:
	if not is_node_ready():
		return
	var now := Time.get_ticks_msec()
	var next_expiry := 0
	for key: String in _changed_roles.keys():
		if _changed_roles[key] <= now:
			_changed_roles.erase(key)
	# Each entry: [text, hand glyph, boxed, action for the shortcut hint].
	var want: Array = []
	for entry: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"takeable_roles")):
		var role := Kit.as_dict(entry.get("role"))
		var key := "%d:%d" % [Kit.get_int(role, "kind", -1), Kit.get_int(role, "index", 0)]
		var player := Kit.get_int(entry, "holder", Bp.HOLDER_OFFICER) == Bp.HOLDER_PLAYER
		var boxed := _changed_roles.has(key)
		if not player and not boxed:
			continue
		var name := _Text.resolve(entry.get("name", {}))
		want.append([tr(&"hud_shell.role.player" if player else &"hud_shell.role.officer").format([name]), player, boxed, StringName(Kit.get_str(entry, "action"))])
		if boxed:
			var until := _changed_roles[key]
			next_expiry = until if next_expiry == 0 else mini(next_expiry, until)
	# Only after a group change: while a group is held its own entry already says which, and a
	# standing note would sit over the horizon where the reticle works.
	if _groups.size() >= 2 and now < _group_note_until_ms:
		var name := tr(&"hud_shell.group.name").format([_number(_current_group + 1)])
		var boxed := now < _group_note_until_ms
		want.append([tr(&"hud_shell.group.current").format([name]), false, boxed, &"take_group_current"])
		if boxed:
			next_expiry = _group_note_until_ms if next_expiry == 0 else mini(next_expiry, _group_note_until_ms)
	_strip_expiry_ms = next_expiry
	# Board and mounts change at 10 Hz; the strip is rebuilt only when what it says changes.
	if want == _strip_shown:
		return
	_strip_shown = want
	Kit.clear(_role_strip)
	for e: Array in want:
		_role_strip.add_child(_strip_entry(e[0], e[1], e[2], e[3]))
	_role_strip.visible = not want.is_empty()
	_apply_backing()


func _strip_entry(text: String, hand: bool, boxed: bool, action: StringName) -> Control:
	var panel := PanelContainer.new()
	panel.mouse_filter = Control.MOUSE_FILTER_IGNORE
	var margin := MarginContainer.new()
	margin.mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_margins(margin, 1)
	panel.add_child(margin)
	var row := HBoxContainer.new()
	row.mouse_filter = Control.MOUSE_FILTER_IGNORE
	Ui.set_separation(row, 1)
	margin.add_child(row)
	if hand:
		var glyph := TextureRect.new()
		glyph.texture = Ui.icon(&"hand")
		glyph.custom_minimum_size = Vector2(24, 24)
		glyph.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
		glyph.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
		glyph.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		glyph.mouse_filter = Control.MOUSE_FILTER_IGNORE
		row.add_child(glyph)
	var label := Kit.label(text, &"Body", true)
	# One line; in a narrow centre (a large UI scale) it ends in an ellipsis and reads in full in
	# the entry's accessible name.
	label.autowrap_mode = TextServer.AUTOWRAP_OFF
	label.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	var font := label.get_theme_font(&"font")
	var full := font.get_string_size(text, HORIZONTAL_ALIGNMENT_LEFT, -1, label.get_theme_font_size(&"font_size")).x if font != null else 200.0
	label.custom_minimum_size = Vector2(ceilf(minf(full, maxf(_strip_text_w, 80.0))), 0.0)
	label.mouse_filter = Control.MOUSE_FILTER_IGNORE
	row.add_child(label)
	if action != &"" and InputMap.has_action(action):
		var hint := ShortcutHint.new()
		hint.compact = true
		hint.action = action
		hint.size_flags_vertical = Control.SIZE_SHRINK_CENTER
		hint.mouse_filter = Control.MOUSE_FILTER_IGNORE
		row.add_child(hint)
	if boxed:
		var box := ModeBox.new()
		panel.add_child(box)
	panel.accessibility_name = text
	return panel


## The 2 px ui.box frame of a change (L1649), drawn over its entry for the mode-change time.
class ModeBox extends Control:
	func _init() -> void:
		mouse_filter = Control.MOUSE_FILTER_IGNORE
		set_anchors_preset(Control.PRESET_FULL_RECT)

	func _notification(what: int) -> void:
		if what == NOTIFICATION_THEME_CHANGED or what == NOTIFICATION_RESIZED:
			queue_redraw()

	func _draw() -> void:
		var w := 2.0
		draw_rect(Rect2(Vector2(w * 0.5, w * 0.5), size - Vector2(w, w)), Ui.colour(&"ui.box"), false, w)


# --- per frame -----------------------------------------------------------------------------------

func _process(delta: float) -> void:
	if _strip_expiry_ms > 0 and Time.get_ticks_msec() >= _strip_expiry_ms:
		_refresh_strip()
	if _focus_to_release != null:
		_release_clicked_focus()
	if not _input_live():
		if _helm_sent != _held_helm() or _trigger or not _roles_down.is_empty():
			_release_all_input()
		return
	# A release the HUD never saw (a control took it, or the window lost it) still lets go.
	for a: StringName in AXES:
		if _strength[a] > 0.0 and not Input.is_action_pressed(a):
			_strength[a] = 0.0
	for action: StringName in _roles_down.keys():
		if not _action_still_down(action):
			_role_edge(action, false)
	if _fire_down_ms >= 0 and not _trigger and Time.get_ticks_msec() - _fire_down_ms >= FIRE_HOLD_MS:
		if _weapon_held and _view == Bp.VIEW_BRIDGE:
			_set_trigger(true)
		else:
			_fire_down_ms = -1
	var stick := _strength[&"helm_ahead"] - _strength[&"helm_astern"]
	if stick != 0.0:
		_seize_telegraph()
		_telegraph = clampf(_telegraph + stick * TELEGRAPH_STICK_RATE * delta, -1.0, 1.0)
	var helm := Vector2(_telegraph if _helm_mine else 0.0, _strength[&"helm_starboard"] - _strength[&"helm_port"])
	if helm != _helm_sent:
		_helm_sent = helm
		Kit.call_on(&"Orders", &"set_helm_input", [helm.x, helm.y])
	var look := Vector2(_strength[&"look_right"] - _strength[&"look_left"], _strength[&"look_down"] - _strength[&"look_up"])
	if look != Vector2.ZERO:
		if _view == Bp.VIEW_PLOT:
			if _plot_table.has_method(&"move_cursor"):
				_plot_table.call(&"move_cursor", look * CURSOR_PX_S * delta)
		else:
			# Aiming by hand on a gamepad: the right stick moves the reticle like a pointer and turns
			# the camera only when pushed past the side of the view.
			if _weapon_held and Ui.using_gamepad and not Ui.using_mouse and _reticle.has_method(&"nudge_pad"):
				var c := _centre.get_global_rect()
				var floor_y := _bottom.get_global_rect().position.y if _bottom.is_visible_in_tree() else c.end.y
				var bounds := Rect2(c.position, Vector2(c.size.x, maxf(floor_y - c.position.y, 0.0)))
				if bounds != _aim_bounds:
					_aim_bounds = bounds
					_reticle.call(&"set_aim_bounds", bounds)
				look = _reticle.call(&"nudge_pad", look, delta)
			var speed := _setting_float(&"input.stick_orbit_speed", 1.0)
			var invert := -1.0 if _setting_bool(&"input.invert_orbit_y") else 1.0
			_orbit(look.x * STICK_ORBIT_DEG_S * speed * delta, look.y * STICK_ORBIT_DEG_S * speed * delta * invert)


func _action_still_down(action: StringName) -> bool:
	if action == &"take_group_current" and _lt_down:
		return true
	return Input.is_action_pressed(action)


## Orders go out only while the sortie or replay is live, this HUD is on screen and nothing is
## pushed over it.
func _input_live() -> bool:
	if not _live or not is_inside_tree() or not visible:
		return false
	var screens := Kit.autoload(&"Screens")
	if screens != null:
		if screens.has_method(&"modal_count") and int(screens.call(&"modal_count")) > 0:
			return false
		if screens.has_method(&"top") and screens.call(&"top") != self and int(screens.call(&"depth")) > 1:
			return false
	return true


func _on_stack(_depth: int) -> void:
	if not _input_live():
		_release_all_input()


func _on_modal(open: int) -> void:
	if open > 0:
		_release_all_input()


## Let go of everything held: role keys, helm axes, the trigger and any drag.
func _release_all_input() -> void:
	for action: StringName in _roles_down.keys():
		_role_edge(action, false)
	_roles_down.clear()
	_lt_down = false
	for a: StringName in AXES:
		_strength[a] = 0.0
	# The rudder centres; the telegraph keeps the engine order it was given.
	if _helm_sent != _held_helm():
		_helm_sent = _held_helm()
		Kit.call_on(&"Orders", &"set_helm_input", [_helm_sent.x, _helm_sent.y])
	_set_trigger(false)
	_fire_down_ms = -1
	_rt_down = false
	_end_drag()
	if _compression_hold_active:
		_compression_hold_active = false
		Kit.call_on(&"Session", &"request_time_rate", [1])


## The helm input with every key up: the telegraph while the player has the helm, else nothing.
func _held_helm() -> Vector2:
	return Vector2(_telegraph if _helm_mine else 0.0, 0.0)


func _end_drag() -> void:
	_press = {}
	_orbit_drag = false


# --- input ---------------------------------------------------------------------------------------

## True when the dry dock owns this event during a Splash: every mouse event, and every key or
## button that also triggers an `editor_*` action exactly.
func shares_input_with_dock(event: InputEvent) -> bool:
	if not _splash_dock_open():
		return false
	if event is InputEventMouse or event is InputEventGesture:
		return true
	if event is InputEventKey:
		# Esc is also the dock's cancel, but the dock takes no keys during a splash: here it opens the
		# splash's pause menu, as in a sortie.
		if event.is_action(&"menu", true):
			return false
		for a: StringName in _editor_actions:
			if event.is_action(a, true):
				return true
	return false


func _splash_dock_open() -> bool:
	if _kind != Bp.KIND_SPLASH:
		return false
	var editor := Kit.autoload(&"Editor")
	return editor != null and editor.has_method(&"is_open") and bool(editor.call(&"is_open"))


func _unhandled_input(event: InputEvent) -> void:
	if not _input_live():
		return
	for a: StringName in DOCK_ONLY:
		if event.is_action(a, true):
			return
	if shares_input_with_dock(event):
		return
	if _handle(event):
		get_viewport().set_input_as_handled()
	elif _nav_without_focus(event):
		# Arrows, the D-pad and the left stick drive the sortie; with nothing focused they must not
		# pull focus into a panel either.
		get_viewport().set_input_as_handled()


func _nav_without_focus(event: InputEvent) -> bool:
	if get_viewport().gui_get_focus_owner() != null:
		return false
	for a: StringName in UI_NAV:
		if event.is_action(a):
			return true
	return false


## One input event; true when it was used.
func _handle(event: InputEvent) -> bool:
	if event is InputEventMouseButton:
		return _mouse_button(event as InputEventMouseButton)
	if event is InputEventMouseMotion:
		return _mouse_motion(event as InputEventMouseMotion)
	if event is InputEventPanGesture:
		if _view != Bp.VIEW_BRIDGE:
			return false
		var d := (event as InputEventPanGesture).delta
		var invert := -1.0 if _setting_bool(&"input.invert_orbit_y") else 1.0
		_orbit(d.x * PAN_DEG_PER_UNIT, d.y * PAN_DEG_PER_UNIT * invert)
		return true
	if event is InputEventMagnifyGesture:
		if _view != Bp.VIEW_BRIDGE:
			return false
		var f := (event as InputEventMagnifyGesture).factor
		if is_finite(f) and f > 0.0:
			_pinch *= f
			if _pinch >= PINCH_STEP:
				_pinch = 1.0
				_zoom(1)
			elif _pinch <= 1.0 / PINCH_STEP:
				_pinch = 1.0
				_zoom(-1)
		return true
	var used := false
	for a: StringName in [&"helm_ahead", &"helm_astern"]:
		if event.is_action(a) and not event is InputEventJoypadMotion:
			# A key or button steps the telegraph once per press; holding it does not run it on.
			if event.is_pressed() and not event.is_echo():
				_step_telegraph(1 if a == &"helm_ahead" else -1)
			return true
	for a: StringName in AXES:
		if event.is_action(a):
			_strength[a] = event.get_action_strength(a) if event.is_pressed() or event is InputEventJoypadMotion else 0.0
			used = true
	if used:
		return true
	if event is InputEventJoypadMotion:
		return _trigger_axes(event as InputEventJoypadMotion)
	return _buttons(event)


func _mouse_button(e: InputEventMouseButton) -> bool:
	if _view != Bp.VIEW_BRIDGE:
		return false
	if e.is_action(&"zoom_in") or e.is_action(&"zoom_out"):
		if e.pressed:
			_zoom(1 if e.is_action(&"zoom_in") else -1)
		return true
	if _weapon_held and e.is_action(&"fire"):
		_fire_edge(e.pressed)
		return true
	if e.is_action(&"camera_orbit"):
		_orbit_drag = e.pressed
		return true
	for action: StringName in [&"designate", &"set_course"]:
		if not e.is_action(action):
			continue
		if e.pressed:
			_press = {"action": action, "pos": e.position, "moved": false}
			return true
		if StringName(_press.get("action", &"")) != action:
			return false
		var moved := bool(_press.get("moved", false))
		_press = {}
		if not moved:
			if action == &"designate":
				_designate_at(e.position)
			else:
				_course_at(e.position, e.shift_pressed)
		return true
	return false


func _mouse_motion(e: InputEventMouseMotion) -> bool:
	if _view != Bp.VIEW_BRIDGE:
		return false
	if not _press.is_empty() and not bool(_press.get("moved", false)):
		if (e.position - Vector2(_press.get("pos", e.position))).length() >= CLICK_SLOP_PX:
			_press["moved"] = true
	var drag := _orbit_drag or (not _press.is_empty() and bool(_press.get("moved", false)) and StringName(_press.get("action", &"")) == &"designate")
	if not drag:
		return false
	var speed := _setting_float(&"input.mouse_orbit_speed", 1.0)
	var invert := -1.0 if _setting_bool(&"input.invert_orbit_y") else 1.0
	_orbit(e.relative.x * ORBIT_DEG_PER_PX * speed, e.relative.y * ORBIT_DEG_PER_PX * speed * invert)
	return true


func _trigger_axes(e: InputEventJoypadMotion) -> bool:
	if e.is_action(&"take_group_current"):
		var s := e.get_action_strength(&"take_group_current")
		if not _lt_down and s >= TRIGGER_ON:
			_lt_down = true
			_role_edge(&"take_group_current", true)
		elif _lt_down and s <= TRIGGER_OFF:
			_lt_down = false
			_role_edge(&"take_group_current", false)
		return true
	if e.is_action(&"fire"):
		var s := e.get_action_strength(&"fire")
		if not _rt_down and s >= TRIGGER_ON:
			_rt_down = true
			if _weapon_held and _view == Bp.VIEW_BRIDGE:
				_fire_edge(true)
		elif _rt_down and s <= TRIGGER_OFF:
			_rt_down = false
			_fire_edge(false)
		return true
	return false


## Keys and gamepad buttons.
func _buttons(event: InputEvent) -> bool:
	var role := _role_action_of(event)
	if role != &"":
		if event.is_echo():
			return true
		_role_edge(role, event.is_pressed())
		return true
	if event.is_action_released(&"time_faster", true) and _compression_hold_active:
		_compression_hold_active = false
		Kit.call_on(&"Session", &"request_time_rate", [1])
		return true
	if not event.is_pressed() or event.is_echo():
		return false
	if _pressed(event, &"view_switch"):
		toggle_view()
	elif _pressed(event, &"menu"):
		# Esc closes the innermost thing first: an open station, then the pause menu opens.
		if _station_name != &"" and not event is InputEventJoypadButton:
			close_station()
		else:
			_open_menu()
	elif _in_replay() and (_pressed(event, &"pause") or _pressed(event, &"time_faster") or _pressed(event, &"time_slower")):
		# In a replay these drive playback; the replay controls own them (the session's pause and time
		# rate do nothing there).
		return false
	elif _pressed(event, &"pause"):
		Kit.call_on(&"Session", &"set_paused", [not _paused])
	elif _pressed(event, &"time_faster"):
		_time_faster()
	elif _pressed(event, &"time_slower"):
		Kit.call_on(&"Session", &"step_time_rate", [-1])
		if _pacing.has_method(&"note_step"):
			_pacing.call(&"note_step", -1)
	elif _pressed(event, &"group_next"):
		_step_group(1)
	elif _pressed(event, &"group_prev"):
		_step_group(-1)
	elif _pressed(event, &"track_next"):
		Kit.call_on(&"Hud", &"select_step", [1])
	elif _pressed(event, &"track_prev"):
		Kit.call_on(&"Hud", &"select_step", [-1])
	elif _pressed(event, &"alert_acknowledge"):
		Kit.call_on(&"Orders", &"acknowledge", [Bp.NONE])
	elif _pressed(event, &"alert_silence"):
		Kit.call_on(&"Orders", &"silence_alerts")
	elif _pressed(event, &"emcon_cycle"):
		_cycle_emcon()
	elif _pressed(event, &"decoys"):
		_fire_decoys()
	elif _pressed(event, &"log_toggle"):
		open_station(&"log")
	elif _pressed(event, &"zoom_in") or _pressed(event, &"zoom_out"):
		_zoom(1 if event.is_action(&"zoom_in") else -1)
	elif _pressed(event, &"cancel"):
		_cancel(event is InputEventJoypadButton)
	elif event is InputEventJoypadButton and _pressed(event, &"designate"):
		_pad_designate()
	elif event is InputEventJoypadButton and _pressed(event, &"set_course"):
		_pad_course()
	elif _pressed(event, &"ui_focus_prev"):
		_enter_panels(false)
	else:
		return false
	return true


func _pressed(event: InputEvent, action: StringName) -> bool:
	return InputMap.has_action(action) and event.is_action_pressed(action, false, true)


## The role action this event presses or releases: the fixed role keys and the current-group key.
func _role_action_of(event: InputEvent) -> StringName:
	if event is InputEventJoypadMotion or event is InputEventMouseMotion:
		return &""
	for action: StringName in [&"take_helm", &"take_sensors", &"take_group_1", &"take_group_2", &"take_group_3", &"take_group_4", &"take_group_current"]:
		if InputMap.has_action(action) and event.is_action(action, true):
			return action
	return &""


## Send one role key edge. The press decides the role (the current group can change while F is
## down), and the release goes to the same role.
func _role_edge(action: StringName, pressed: bool) -> void:
	if pressed:
		if _roles_down.has(action):
			return
		var role := _role_for(action)
		if role.is_empty():
			return
		_roles_down[action] = role
		Kit.call_on(&"Orders", &"role_key", [role[0], role[1], true])
	else:
		if not _roles_down.has(action):
			return
		var role: Array = _roles_down[action]
		_roles_down.erase(action)
		Kit.call_on(&"Orders", &"role_key", [role[0], role[1], false])


## [kind, index] of the role an action takes, or [] when the hull has no such role.
func _role_for(action: StringName) -> Array:
	match action:
		&"take_helm":
			return [Bp.ROLE_HELM, 0]
		&"take_sensors":
			return [Bp.ROLE_SENSORS, 0]
		&"take_group_current":
			return [Bp.ROLE_WEAPON_GROUP, _current_group] if _groups.has(_current_group) else []
	var s := String(action)
	if s.begins_with("take_group_"):
		var g := s.trim_prefix("take_group_").to_int() - 1
		return [Bp.ROLE_WEAPON_GROUP, g] if _groups.has(g) else []
	return []


## Take the helm for the telegraph if the officer has it, starting from the speed he ordered so the
## first step does not ring the engines down to stop.
func _seize_telegraph() -> void:
	if _helm_mine:
		return
	_telegraph = _officer_telegraph
	_helm_mine = true
	_helm_seized_ms = Time.get_ticks_msec()
	Kit.call_on(&"Orders", &"take_control", [Bp.ROLE_HELM, 0, true])


## While the officer has the helm, remember his ordered speed for the telegraph to start from.
func _on_motion() -> void:
	if _helm_mine:
		return
	# The motion of the snapshot that first shows the player at the helm already carries his
	# zero throttle: read the holder from the same snapshot, not the board signal that follows.
	var holder: Variant = Kit.call_on(&"Hud", &"role_holder", [Bp.ROLE_HELM, 0])
	if holder is int and int(holder) == Bp.HOLDER_OFFICER:
		_officer_telegraph = _ordered_telegraph()


## The officer's ordered speed as a telegraph setting, to the nearest step.
func _ordered_telegraph() -> float:
	var motion := Kit.as_dict(Kit.call_on(&"Hud", &"motion"))
	var limits := Kit.as_dict(Kit.call_on(&"Hud", &"motion_limits"))
	var top := Kit.get_float(limits, "max_speed_kn", 0.0)
	var ordered := Kit.get_float(motion, "ordered_speed_kn", 0.0)
	return clampf(snappedf(ordered / top, TELEGRAPH_STEP), -1.0, 1.0) if top > 0.0 else 0.0


func _step_telegraph(dir: int) -> void:
	_seize_telegraph()
	_telegraph = clampf(snappedf(_telegraph, TELEGRAPH_STEP) + TELEGRAPH_STEP * dir, -1.0, 1.0)


## The fire button or trigger, pressed or released while a weapon role is held. A pull longer than
## FIRE_HOLD_MS holds the trigger (fire as the mount bears, until released); a shorter click sends one
## aimed shot (`Orders.fire_at_reticle`), which the server keeps until the mount has trained onto
## the reticle. A held trigger fires only once the mount is on the aim, so before this a quick click
## while the mount was still training fired nothing.
func _fire_edge(pressed: bool) -> void:
	if pressed:
		_fire_down_ms = Time.get_ticks_msec()
		return
	if _trigger:
		_set_trigger(false)
	elif _fire_down_ms >= 0 and not _weapon_role.is_empty():
		Kit.call_on(&"Orders", &"fire_at_reticle", [_weapon_role[0], _weapon_role[1]])
	_fire_down_ms = -1


## The speed a course order carries: -1 keeps the ordered speed, but a ship ordered to stop (as
## every sortie starts) would take the course and never move, so then half the top speed. The plot
## table's course orders use it too.
func course_speed() -> float:
	var ordered := Kit.get_float(Kit.as_dict(Kit.call_on(&"Hud", &"motion")), "ordered_speed_kn", 0.0)
	if ordered >= COURSE_MIN_KN:
		return -1.0
	var top := Kit.get_float(Kit.as_dict(Kit.call_on(&"Hud", &"motion_limits")), "max_speed_kn", 0.0)
	return top * COURSE_DEFAULT_SHARE if top > 0.0 else -1.0


func _set_trigger(on: bool) -> void:
	if on == _trigger:
		return
	_trigger = on
	Kit.call_on(&"Orders", &"set_trigger", [on])


func _time_faster() -> void:
	var coop := _net != Bp.NET_OFFLINE and _kind != Bp.KIND_PVP
	if coop and _setting_bool(&"input.compression_hold"):
		_compression_hold_active = true
		Kit.call_on(&"Session", &"request_time_rate", [4])
		return
	Kit.call_on(&"Session", &"step_time_rate", [1])
	# The bar explains a step the server holds back ("Time held at 1x: ...").
	if _pacing.has_method(&"note_step"):
		_pacing.call(&"note_step", 1)


func _in_replay() -> bool:
	return _kind == Bp.KIND_REPLAY or _state == Bp.SESSION_REPLAY


func _open_menu() -> void:
	var screens := Kit.autoload(&"Screens")
	if screens == null:
		return
	var context: StringName = &"sortie"
	if _kind == Bp.KIND_REPLAY or _state == Bp.SESSION_REPLAY:
		context = &"replay"
	elif _kind == Bp.KIND_SPLASH:
		context = &"splash"
	_release_all_input()
	screens.call(&"push", &"pause_menu", {"context": context})


func _cycle_emcon() -> void:
	var e := Kit.as_dict(Kit.call_on(&"Hud", &"emcon"))
	var now := Kit.get_int(e, "emcon", Bp.EMCON_ACTIVE)
	var order: Array[int] = [Bp.EMCON_SILENT, Bp.EMCON_BURST, Bp.EMCON_ACTIVE]
	var i := order.find(now)
	Kit.call_on(&"Orders", &"set_emcon", [order[(i + 1) % order.size()] if i >= 0 else Bp.EMCON_ACTIVE])


## Decoy launchers are the mounts that belong to no weapons group.
func _fire_decoys() -> void:
	for m: Dictionary in Kit.dicts(Kit.call_on(&"Hud", &"mounts")):
		if Kit.get_int(m, "group", 0) < 0:
			Kit.call_on(&"Orders", &"fire_decoy", [Kit.get_int(m, "slot", Bp.NONE)])


func _cancel(pad: bool) -> void:
	if _owns_focus():
		get_viewport().gui_release_focus()
		_hud_focus_by_pad = false
		return
	if _view == Bp.VIEW_PLOT and _plot_table.has_method(&"cancel_tool") and bool(_plot_table.call(&"cancel_tool")):
		return
	if _station_name != &"":
		close_station()
		return
	var sel: Variant = Kit.call_on(&"Hud", &"selected_track")
	if sel is int and int(sel) != Bp.NONE:
		Kit.call_on(&"Hud", &"select_track", [Bp.NONE])
		return
	if pad:
		_enter_panels(true)


## Move focus into the docked panels on purpose: the track list first (the most used panel).
func _enter_panels(pad: bool) -> void:
	# The alert strip and the weapons panel show and rebuild on their own: link what is there now.
	_link_panels()
	var target: Control = null
	if _track_list.has_method(&"default_focus"):
		var v: Variant = _track_list.call(&"default_focus")
		if v is Control:
			target = v
	if target == null:
		target = Screens.first_focusable(_left) if Kit.autoload(&"Screens") != null else null
	if target != null:
		_hud_focus_by_pad = pad
		target.grab_focus()


func _owns_focus() -> bool:
	var f := get_viewport().gui_get_focus_owner()
	return f != null and _frame.is_ancestor_of(f)


## A mouse click on a HUD control gives focus back to the sortie once the button is up, so keys
## keep driving the ship (a button that lost focus mid-press would drop the click); text fields
## keep it. Keyboard and gamepad focus stays where the player put it.
func _on_focus_changed(control: Control) -> void:
	_focus_to_release = null
	if control == null or not _frame.is_ancestor_of(control):
		return
	if not Ui.using_mouse or Ui.using_gamepad:
		return
	if control is LineEdit or control is TextEdit or control is SpinBox:
		return
	_focus_to_release = weakref(control)


func _release_clicked_focus() -> void:
	var c: Object = _focus_to_release.get_ref() if _focus_to_release != null else null
	if c == null or not is_instance_valid(c):
		_focus_to_release = null
		return
	if Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT) or Input.is_mouse_button_pressed(MOUSE_BUTTON_RIGHT) or Input.is_mouse_button_pressed(MOUSE_BUTTON_MIDDLE):
		return
	_focus_to_release = null
	if (c as Control).has_focus():
		(c as Control).release_focus()


# --- orders --------------------------------------------------------------------------------------

func _designate_at(pos: Vector2) -> void:
	# A track marker or its label, on the view or at its edge, takes the click before the 3D pick.
	var marked := int(_overlay.call(&"track_at", pos)) if _overlay.has_method(&"track_at") else 0
	if marked > 0:
		Kit.call_on(&"Hud", &"select_track", [marked])
		Kit.call_on(&"Orders", &"designate", [marked])
		return
	var w := world_view()
	if w == null or not w.has_method(&"pick"):
		return
	var hit := Kit.as_dict(w.call(&"pick", pos))
	match StringName(Kit.get_str(hit, "kind", "none")):
		&"track":
			var t := Kit.get_int(hit, "track", Bp.NONE)
			if t > 0:
				Kit.call_on(&"Hud", &"select_track", [t])
				Kit.call_on(&"Orders", &"designate", [t])
		&"sea":
			Kit.call_on(&"Hud", &"select_track", [Bp.NONE])


func _course_at(pos: Vector2, append: bool) -> void:
	var w := world_view()
	# Right-click on a part of the own hull asks Why? of that part (L353) instead of setting course.
	if w != null and w.has_method(&"pick") and _why_popup.has_method(&"ask"):
		var hit := Kit.as_dict(w.call(&"pick", pos))
		if StringName(hit.get("kind", &"")) == &"own" and Kit.get_int(hit, "slot", Bp.NONE) != Bp.NONE:
			_why_popup.call(&"ask", Kit.get_int(hit, "slot", Bp.NONE), pos)
			return
	if w == null or not w.has_method(&"sea_point"):
		return
	var p: Variant = w.call(&"sea_point", pos)
	if p is Vector2 and (p as Vector2).is_finite():
		Kit.call_on(&"Orders", &"set_course", [PackedVector2Array([p]), course_speed(), append])


func _pad_designate() -> void:
	var sel: Variant = Kit.call_on(&"Hud", &"selected_track")
	if sel is int and int(sel) > 0:
		Kit.call_on(&"Orders", &"designate", [int(sel)])
		return
	if _view == Bp.VIEW_BRIDGE:
		_designate_at(_frame.get_rect().get_center())


## X on a gamepad in the bridge view: a course along the camera's bearing (point the view where
## you want to go). On the plot the plot cursor's own handler takes it first.
func _pad_course() -> void:
	if _view != Bp.VIEW_BRIDGE:
		return
	var w := world_view()
	if w == null or not w.has_method(&"orbit_state"):
		return
	var orbit := Kit.as_dict(w.call(&"orbit_state"))
	var motion := Kit.as_dict(Kit.call_on(&"Hud", &"motion"))
	var own: Variant = motion.get("pos")
	if not own is Vector2:
		return
	var yaw := deg_to_rad(Kit.get_float(orbit, "yaw_deg", 0.0))
	var at := (own as Vector2) + Vector2(sin(yaw), -cos(yaw)) * PAD_COURSE_M
	Kit.call_on(&"Orders", &"set_course", [PackedVector2Array([at]), course_speed(), false])


func _orbit(yaw_deg: float, pitch_deg: float) -> void:
	var w := world_view()
	if w != null and w.has_method(&"orbit") and is_finite(yaw_deg) and is_finite(pitch_deg):
		w.call(&"orbit", yaw_deg, pitch_deg)


func _zoom(steps: int) -> void:
	if _view == Bp.VIEW_PLOT:
		if _plot_table.has_method(&"zoom"):
			_plot_table.call(&"zoom", steps)
		return
	var w := world_view()
	if w != null and w.has_method(&"zoom"):
		w.call(&"zoom", float(steps))


# --- helpers -------------------------------------------------------------------------------------

func _setting_bool(key: StringName) -> bool:
	var v: Variant = Kit.call_on(&"Settings", &"get_value", [key])
	return v == true or (v is int and int(v) != 0)


func _setting_float(key: StringName, fallback: float) -> float:
	var v: Variant = Kit.call_on(&"Settings", &"get_value", [key])
	return float(v) if (v is float or v is int) and is_finite(float(v)) else fallback


static func _number(n: int) -> String:
	return TranslationServer.format_number(str(n), TranslationServer.get_locale())
