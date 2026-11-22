extends Node
## Autoload `Screens`. Screen router: change, push and pop screens under res://ui/screens,
## the sortie, dry dock and chart roots, loading cover, modal stack, focus restore for gamepads.
## DESIGN.md L1738 (explicit focus neighbours, grab_focus.call_deferred). Unit: ui-shell.
## Reads and calls: Session.state_changed, Campaign.phase_changed, Editor.is_open.
##
## A screen is named by the stem of its scene under res://ui/screens (`&"main_menu"` is
## res://ui/screens/main_menu.tscn), by one of the roots below, or by a full `res://` path:
## - `&"sortie"`: res://world/world.tscn with res://ui/hud/sortie_hud.tscn over it.
## - `&"replay"`: the sortie root plus res://ui/screens/replay_controls.tscn.
## - `&"dry_dock"`: res://ui/editor/dry_dock.tscn (it stages its own BpEditorView).
## - `&"chart"`: res://ui/chart/chart_table.tscn.
## - `&"consent"`, `&"loading"`: the boot screens in res://boot.
##
## Stack: one base screen (`change`) with screens pushed over it (`push`, `pop`). A pushed screen
## covers the screens below for input: they keep drawing and running (a co-op sortie does not stop
## under the pause menu) but get no clicks or keys. Focus is saved on push and given back on pop,
## so a gamepad player lands where they left (L1738). Esc (`ui_cancel`) over a pushed screen calls its
## `back_requested()` if it has one, otherwise pops it. A base screen handles its own Esc.
##
## A screen root may define (all optional):
## - `setup(args: Dictionary)`: called right after the screen enters the tree, after its `_ready`,
##   with the `args` given to `change` or `push`.
## - `default_focus() -> Control`: the control that takes focus when the screen opens; otherwise the
##   first focusable control in tree order does.
## - `back_requested()`: Esc over this pushed screen; the screen decides (and usually calls `pop`).
##
## Modals (`open_dialog`, `open_message`) sit above every screen in their own stack; each one takes
## all input until it closes. Toasts (`toast`) are for global messages only: "Design saved" and
## connection loss (L1605, L1616). The loading cover (`show_loading`) and the safe-mode notice
## (`set_safe_mode`) sit above everything.
##
## Routing: after boot calls `start_routing()`, session and campaign state move the screens:
## - Session loading (any kind but Splash, Benchmark and a dry-dock tutorial, which their own screens
##   drive) builds the sortie root under the loading cover; the cover lifts when the world view
##   reports `warmed_up`.
##   Replays add the replay controls. A lobby shows `&"lobby"`.
## - A non-campaign sortie that ends pushes `&"sortie_end"` over the world; leaving it (Session goes
##   idle) returns to the main menu. A failed session returns to the screen it started from and
##   shows the failure text in a dialog.
## - Campaign phases: chart, advancing and over show `&"chart"`; briefing `&"briefing"`; dry dock
##   `&"dry_dock"`; after-action `&"after_action"`; a closed campaign goes back to the main menu.
## `change` to the screen already at the base with no args and nothing pushed does nothing, so a
## screen may also navigate itself without fighting the router.

signal screen_changed(name: StringName)
signal stack_changed(depth: int)
signal modal_changed(open: int)

const SCREEN_DIR := "res://ui/screens"
const ROOTS: Dictionary[StringName, String] = {
	&"dry_dock": "res://ui/editor/dry_dock.tscn",
	&"chart": "res://ui/chart/chart_table.tscn",
	&"consent": "res://boot/consent.tscn",
	&"loading": "res://boot/loading.tscn",
}
const WORLD_SCENE := "res://world/world.tscn"
const SORTIE_HUD_SCENE := "res://ui/hud/sortie_hud.tscn"
const REPLAY_CONTROLS_SCENE := "res://ui/screens/replay_controls.tscn"
const LOADING_SCENE := "res://boot/loading.tscn"
const DIALOG_SCENE := "res://ui/widgets/dialog.tscn"
const TOAST_SCENE := "res://ui/widgets/toast.tscn"
const WORLD_ROOTS: Array[StringName] = [&"sortie", &"replay"]
## Bases that belong to a live session; leaving the session leaves them.
const SESSION_BASES: Array[StringName] = [&"sortie", &"replay", &"lobby", &"sortie_end"]
const CAMPAIGN_BASES: Array[StringName] = [&"chart", &"briefing", &"dry_dock", &"after_action"]
## If a world never reports `warmed_up`, the cover lifts after this long in the sortie, so a
## broken warm-up cannot trap the player behind it.
const WARM_UP_LIMIT_S := 20.0
const LAYER_OVERLAY := 60
const LAYER_MODAL := 70
const LAYER_TOAST := 80
const LAYER_COVER := 90
## Session states (Bp.SESSION_*), kept here so this script parses before the extension loads.
const SESSION_IDLE := 0
const SESSION_LOADING := 1
const SESSION_LOBBY := 2
const SESSION_IN_SORTIE := 3
const SESSION_ENDED := 4
const SESSION_REPLAY := 5
const SESSION_FAILED := 6
const KIND_SPLASH := 5
const KIND_TUTORIAL := 6
const KIND_REPLAY := 7
const KIND_BENCHMARK := 8
const PHASE_NONE := 0
const PHASE_CHART := 1
const PHASE_ADVANCING := 2
const PHASE_BRIEFING := 3
const PHASE_DRY_DOCK := 4
const PHASE_AFTER_ACTION := 6
const PHASE_OVER := 7
const DOCK_CAMPAIGN := 1


## Catches input meant for the screens under a pushed one. It sits in tree order after the base
## screens and before the pushed ones, so pushed screens see input first and the base never does.
class InputShield extends Control:
	var router: Node

	func _init() -> void:
		name = &"InputShield"
		set_anchors_preset(Control.PRESET_FULL_RECT)
		mouse_filter = Control.MOUSE_FILTER_STOP
		focus_mode = Control.FOCUS_NONE
		visible = false

	func _unhandled_input(event: InputEvent) -> void:
		if not visible:
			return
		if event.is_action_pressed(&"ui_cancel", false, true):
			router.call(&"back")
		elif router.call(&"_wants_focus", event):
			router.call(&"_focus_top")
		get_viewport().set_input_as_handled()


var routing := false

var _stage: Node
var _overlay: CanvasLayer
var _shield: InputShield
var _modal_layer: CanvasLayer
var _toast_layer: CanvasLayer
var _cover_layer: CanvasLayer
var _loading: Node
var _toast: Node
var _base_name: StringName = &""
var _base_nodes: Array[Node] = []
## Pushed screens, bottom first: `{ "name": StringName, "node": Node, "focus": Control or null }`.
var _pushed: Array[Dictionary] = []
## Open modals, bottom first: `{ "node": Node, "focus": Control or null }`.
var _modals: Array[Dictionary] = []
var _return_to: StringName = &"main_menu"
var _failure_text: Dictionary = {}
var _failure_shown := false
var _world_warm := false
var _warm_timer: SceneTreeTimer
var _session_kind := -1
var _scenes: Dictionary[String, PackedScene] = {}


func _ready() -> void:
	process_mode = Node.PROCESS_MODE_ALWAYS
	_stage = Node.new()
	_stage.name = &"Stage"
	# Screens and the world pause with the tree; menus, dialogs, toasts and the cover never do.
	_stage.process_mode = Node.PROCESS_MODE_PAUSABLE
	add_child(_stage)
	_overlay = _layer(&"Overlay", LAYER_OVERLAY)
	_shield = InputShield.new()
	_shield.router = self
	_overlay.add_child(_shield)
	_modal_layer = _layer(&"Modals", LAYER_MODAL)
	_toast_layer = _layer(&"Toasts", LAYER_TOAST)
	_cover_layer = _layer(&"Cover", LAYER_COVER)
	_loading = _instance(LOADING_SCENE)
	if _loading != null:
		_cover_layer.add_child(_loading)
	_toast = _instance(TOAST_SCENE)
	if _toast != null:
		_toast_layer.add_child(_toast)
	_connect(^"/root/Session", &"state_changed", _on_session_state)
	_connect(^"/root/Session", &"failed", _on_session_failed)
	_connect(^"/root/Session", &"sortie_started", _on_sortie_started)
	_connect(^"/root/Session", &"player_joined", _on_player_joined)
	_connect(^"/root/Session", &"player_dropped", _on_player_dropped)
	_connect(^"/root/Campaign", &"phase_changed", _on_campaign_phase)


func _layer(layer_name: StringName, index: int) -> CanvasLayer:
	var layer := CanvasLayer.new()
	layer.name = layer_name
	layer.layer = index
	add_child(layer)
	return layer


func _connect(path: NodePath, signal_name: StringName, handler: Callable) -> void:
	var node := get_node_or_null(path)
	if node != null and node.has_signal(signal_name) and not node.is_connected(signal_name, handler):
		node.connect(signal_name, handler)


func _unhandled_input(event: InputEvent) -> void:
	# Only base screens are left here (the shield takes input while something is pushed). A gamepad
	# or arrow press with nothing focused gives the base screen its first focus.
	if _wants_focus(event):
		_focus_top()


# --- the stack -----------------------------------------------------------------------------------

## Replace everything on the stack with `screen` (a stem, root name or res:// path). Returns the
## new screen's root node (the HUD for the sortie roots), or null when the scene cannot load.
func change(screen: StringName, args: Dictionary = {}) -> Node:
	if screen == _base_name and args.is_empty() and _pushed.is_empty() and not _base_nodes.is_empty():
		return _base_nodes[-1]
	var nodes := _build(screen)
	if nodes.is_empty():
		push_error("Screens: cannot open %s" % screen)
		return null
	if not (screen in SESSION_BASES) and not (screen in WORLD_ROOTS) and screen != &"loading" and screen != &"consent":
		_return_to = screen
	_clear_pushed()
	for old: Node in _base_nodes:
		if is_instance_valid(old):
			old.queue_free()
	_base_nodes = nodes
	_base_name = screen
	get_viewport().gui_release_focus()
	for node: Node in nodes:
		_stage.add_child(node)
	var root: Node = nodes[-1]
	_setup(root, args)
	_focus_screen.call_deferred(root)
	_shield.visible = false
	screen_changed.emit(screen)
	stack_changed.emit(depth())
	return root


## Open `screen` over the current one. Returns its root node, or null when it cannot load.
func push(screen: StringName, args: Dictionary = {}) -> Node:
	var nodes := _build(screen)
	if nodes.is_empty():
		push_error("Screens: cannot open %s" % screen)
		return null
	var root: Node = nodes[0]
	for extra: Node in nodes.slice(1):
		extra.free()
	var focus_owner := get_viewport().gui_get_focus_owner()
	_pushed.append({"name": screen, "node": root, "focus": focus_owner})
	get_viewport().gui_release_focus()
	_shield.visible = true
	_overlay.add_child(root)
	_setup(root, args)
	_focus_screen.call_deferred(root)
	screen_changed.emit(screen)
	stack_changed.emit(depth())
	return root


## Close the top pushed screen and give focus back to where it was. Does nothing at the base.
func pop() -> void:
	if _pushed.is_empty():
		return
	var entry: Dictionary = _pushed.pop_back()
	var node: Node = entry["node"]
	if is_instance_valid(node):
		node.queue_free()
	_shield.visible = not _pushed.is_empty()
	_restore_focus(entry.get("focus"), top())
	screen_changed.emit(top_name())
	stack_changed.emit(depth())


## Close every screen, base included, leaving only what is under them (a world, or nothing).
func clear() -> void:
	_clear_pushed()
	for old: Node in _base_nodes:
		if is_instance_valid(old):
			old.queue_free()
	_base_nodes.clear()
	_base_name = &""
	get_viewport().gui_release_focus()
	screen_changed.emit(&"")
	stack_changed.emit(0)


## Pop every pushed screen down to the base.
func pop_to_base() -> void:
	while not _pushed.is_empty():
		pop()


## What Esc does over a pushed screen: the screen's own `back_requested()`, else `pop()`. Returns
## false when only the base is open.
func back() -> bool:
	if _pushed.is_empty():
		return false
	var node: Node = _pushed[-1]["node"]
	if is_instance_valid(node) and node.has_method(&"back_requested"):
		node.call(&"back_requested")
	else:
		pop()
	return true


## The top screen's root node: the last pushed one, else the base (the HUD on a sortie root).
func top() -> Node:
	if not _pushed.is_empty():
		return _pushed[-1]["node"]
	return _base_nodes[-1] if not _base_nodes.is_empty() else null


func top_name() -> StringName:
	return _pushed[-1]["name"] if not _pushed.is_empty() else _base_name


func base_name() -> StringName:
	return _base_name


## 1 for the base alone, plus one per pushed screen; 0 before the first `change`.
func depth() -> int:
	return (1 if not _base_nodes.is_empty() else 0) + _pushed.size()


## True when `screen` is the base or any pushed screen.
func is_open(screen: StringName) -> bool:
	if screen == _base_name:
		return true
	for entry: Dictionary in _pushed:
		if entry["name"] == screen:
			return true
	return false


## The sortie world's BpWorldView while a sortie or replay root is the base, else null.
func world_view() -> Node:
	if not (_base_name in WORLD_ROOTS):
		return null
	for node: Node in _base_nodes:
		if is_instance_valid(node):
			var found := _find_world_view(node)
			if found != null:
				return found
	return null


## Open the dry dock: `Editor.open(mode, options)`, then the dock screen unless an issue is fatal.
## Returns the `Array[IssueDict]` from `Editor.open` so the caller can show them.
func open_dry_dock(mode: int, options: Dictionary = {}) -> Array:
	var editor := get_node_or_null(^"/root/Editor")
	if editor == null:
		return []
	var issues: Array = editor.call(&"open", mode, options)
	for issue: Variant in issues:
		if issue is Dictionary and bool((issue as Dictionary).get("fatal", false)):
			return issues
	change(&"dry_dock")
	return issues


func _build(screen: StringName) -> Array[Node]:
	var out: Array[Node] = []
	if screen in WORLD_ROOTS:
		var world := _instance(WORLD_SCENE)
		var hud := _instance(SORTIE_HUD_SCENE)
		if world == null or hud == null:
			for n: Node in [world, hud]:
				if n != null:
					n.free()
			return out
		out.append(world)
		if screen == &"replay":
			var replay := _instance(REPLAY_CONTROLS_SCENE)
			if replay != null:
				var layer := CanvasLayer.new()
				layer.name = &"ReplayLayer"
				layer.layer = 2
				layer.add_child(replay)
				out.append(layer)
		out.append(hud)
		return out
	var node := _instance(scene_path(screen))
	if node != null:
		out.append(node)
	return out


## The scene file behind a screen name.
static func scene_path(screen: StringName) -> String:
	var s := String(screen)
	if s.begins_with("res://"):
		return s
	if ROOTS.has(screen):
		return ROOTS[screen]
	return "%s/%s.tscn" % [SCREEN_DIR, s]


func _instance(path: String) -> Node:
	var scene: PackedScene = _scenes.get(path)
	if scene == null:
		if not ResourceLoader.exists(path):
			return null
		scene = load(path) as PackedScene
		if scene == null:
			return null
		_scenes[path] = scene
	return scene.instantiate()


func _setup(root: Node, args: Dictionary) -> void:
	if root.has_method(&"setup"):
		root.call(&"setup", args)


func _clear_pushed() -> void:
	for entry: Dictionary in _pushed:
		var node: Node = entry["node"]
		if is_instance_valid(node):
			node.queue_free()
	_pushed.clear()
	_shield.visible = false


static func _find_world_view(node: Node) -> Node:
	if node.is_class("BpWorldView"):
		return node
	for child: Node in node.get_children():
		var found := _find_world_view(child)
		if found != null:
			return found
	return null


# --- focus ---------------------------------------------------------------------------------------

func _focus_screen(root: Node) -> void:
	if not is_instance_valid(root) or not root.is_inside_tree():
		return
	var owner_now := get_viewport().gui_get_focus_owner()
	if owner_now != null and root.is_ancestor_of(owner_now):
		return
	Ui.focus(_default_focus(root))


func _restore_focus(saved: Variant, now_top: Node) -> void:
	# Validity first: `is` on a freed instance is a script error (the saved owner can sit in a screen
	# popped in the same frame this one was pushed).
	if is_instance_valid(saved) and saved is Control:
		var control := saved as Control
		if control.is_inside_tree() and control.is_visible_in_tree() and control.focus_mode != Control.FOCUS_NONE:
			Ui.focus(control)
			return
	if now_top != null:
		_focus_screen.call_deferred(now_top)


func _default_focus(root: Node) -> Control:
	if root.has_method(&"default_focus"):
		var chosen: Variant = root.call(&"default_focus")
		if chosen is Control:
			return chosen
	return first_focusable(root)


## The first visible control that takes keyboard focus under `root`, in tree order.
static func first_focusable(root: Node) -> Control:
	if root is Control:
		var c := root as Control
		if not c.visible:
			return null
		if c.focus_mode == Control.FOCUS_ALL and c.is_visible_in_tree():
			return c
	elif root is CanvasItem and not (root as CanvasItem).visible:
		return null
	for child: Node in root.get_children():
		var found := first_focusable(child)
		if found != null:
			return found
	return null


func _wants_focus(event: InputEvent) -> bool:
	if get_viewport().gui_get_focus_owner() != null:
		return false
	for action: StringName in [&"ui_up", &"ui_down", &"ui_left", &"ui_right", &"ui_focus_prev"]:
		if event.is_action_pressed(action, false, true):
			return true
	return false


func _focus_top() -> void:
	if not _modals.is_empty():
		var node: Node = _modals[-1]["node"]
		if is_instance_valid(node):
			Ui.focus(_default_focus(node))
		return
	var root := top()
	if root != null:
		Ui.focus(_default_focus(root))


# --- modals --------------------------------------------------------------------------------------

## Open a modal dialog. `title` and `body` are text keys (`StringName`) or `MsgDict`s; a plain
## `String` is shown as it is (player-typed names only). `buttons` lists the choices in order:
## `{ "id": StringName, "label": StringName (an effect verb, L1697) }`. Esc picks `cancel_id`, or
## does nothing when it is empty. `focus_id` takes focus; give the safe choice for destructive
## questions. Returns the dialog; its `chosen(id)` signal fires once, and the dialog then closes.
func open_dialog(title: Variant, body: Variant, buttons: Array, cancel_id: StringName = &"", focus_id: StringName = &"") -> Node:
	var dialog := _instance(DIALOG_SCENE)
	if dialog == null:
		push_error("Screens: cannot open the dialog scene")
		return null
	var focus_owner := get_viewport().gui_get_focus_owner()
	_modals.append({"node": dialog, "focus": focus_owner})
	get_viewport().gui_release_focus()
	_modal_layer.add_child(dialog)
	dialog.call(&"configure", title, body, buttons, cancel_id, focus_id)
	dialog.connect(&"chosen", _on_dialog_chosen.bind(dialog), CONNECT_ONE_SHOT)
	modal_changed.emit(_modals.size())
	return dialog


## A message with one Close button. Returns the dialog, as `open_dialog` does.
func open_message(title: Variant, body: Variant) -> Node:
	return open_dialog(title, body, [{"id": &"close", "label": &"shell.dialog.close"}], &"close", &"close")


## Close every open modal without a choice (their `chosen` signal fires with `&""`).
func close_modals() -> void:
	for entry: Dictionary in _modals.duplicate():
		var node: Node = entry["node"]
		if is_instance_valid(node) and node.has_method(&"choose"):
			node.call(&"choose", &"")
		if _modals.has(entry):
			_modals.erase(entry)
			if is_instance_valid(node):
				node.queue_free()
	modal_changed.emit(0)


## Number of open modals.
func modal_count() -> int:
	return _modals.size()


func _on_dialog_chosen(_id: StringName, dialog: Node) -> void:
	var index := -1
	for i in _modals.size():
		if _modals[i]["node"] == dialog:
			index = i
	var saved: Variant = null
	if index >= 0:
		saved = _modals[index].get("focus")
		_modals.remove_at(index)
	if is_instance_valid(dialog):
		dialog.queue_free()
	var under: Node = _modals[-1]["node"] if not _modals.is_empty() else top()
	_restore_focus(saved, under)
	modal_changed.emit(_modals.size())


# --- toast, cover, notice ------------------------------------------------------------------------

## Show a global message: a text key or `MsgDict`. Only saves, joins or leaves and connection loss
## use this (L1605); everything a sortie raises goes inline, to the log or to the alert strip.
func toast(message: Variant) -> void:
	if _toast != null:
		_toast.call(&"show_message", message)


## Cover the screen while something loads: `text` is a text key or `MsgDict`, `fraction` 0..1 or
## below 0 when the amount is not known.
func show_loading(text: Variant, fraction: float = -1.0) -> void:
	if _loading != null:
		_loading.call(&"cover", text, fraction)


## Like `show_loading` but as a strip at the bottom that leaves the picture visible.
func show_caption(text: Variant, fraction: float = -1.0) -> void:
	if _loading != null:
		_loading.call(&"caption", text, fraction)


func set_loading_progress(fraction: float) -> void:
	if _loading != null:
		_loading.call(&"set_progress", fraction)


func hide_loading() -> void:
	if _loading != null:
		_loading.call(&"uncover")


func is_loading() -> bool:
	return _loading != null and bool(_loading.call(&"is_covering"))


## Show or hide the persistent safe-mode notice (Compatibility renderer, L1270).
func set_safe_mode(on: bool) -> void:
	if _loading != null:
		_loading.call(&"set_safe_mode", on)


# --- routing -------------------------------------------------------------------------------------

## Let session and campaign state move the screens. Boot calls it once its own screens are done.
func start_routing() -> void:
	routing = true


# --- other players joining and leaving a sortie (L1605: join and leave toast) ------------------------

## Every hull is announced as joined when a sortie starts; only joins after that are news.
const JOIN_TOAST_AFTER_MS := 5000
var _sortie_started_ms := -1
## Names by player slot from this sortie's joins: a player who left is gone from the lobby by the time
## their leaving arrives.
var _player_names: Dictionary[int, String] = {}


func _on_sortie_started() -> void:
	_sortie_started_ms = Time.get_ticks_msec()


func _on_player_joined(player: int, player_name: String) -> void:
	_player_names[player] = player_name
	if _sortie_started_ms < 0 or Time.get_ticks_msec() - _sortie_started_ms < JOIN_TOAST_AFTER_MS:
		return
	# The player's own hull is no news to them (a splash basin announces it again on every Splash).
	var session := get_node_or_null(^"/root/Session")
	var info: Variant = session.call(&"info") if session != null and session.has_method(&"info") else {}
	if info is Dictionary and int((info as Dictionary).get("player", -1)) == player:
		return
	toast({"key": &"conn.toast.joined", "args": [player_name]})


func _on_player_dropped(player: int, officers_in_command: bool) -> void:
	var player_name := _player_name(player)
	toast({"key": &"conn.toast.dropped" if officers_in_command else &"conn.toast.left", "args": [player_name]})


## A player's name by slot: from this sortie's joins, else the lobby, else a neutral word.
func _player_name(player: int) -> String:
	if not _player_names.get(player, "").is_empty():
		return _player_names[player]
	var lobby := get_node_or_null(^"/root/Lobby")
	if lobby != null:
		var view: Variant = lobby.call(&"lobby")
		if view is Dictionary:
			for p: Variant in (view as Dictionary).get("players", []):
				if p is Dictionary and int((p as Dictionary).get("slot", -1)) == player:
					return String((p as Dictionary).get("name", ""))
	return tr(&"conn.toast.someone")


func _session_info_kind() -> int:
	var session := get_node_or_null(^"/root/Session")
	if session == null:
		return -1
	var info: Variant = session.call(&"info")
	if info is Dictionary:
		return int((info as Dictionary).get("kind", -1))
	return -1


func _campaign_active() -> bool:
	var campaign := get_node_or_null(^"/root/Campaign")
	return campaign != null and bool(campaign.call(&"is_active"))


## Splash, the benchmark and a tutorial run from the dry dock play inside their own screens.
func _owned_elsewhere() -> bool:
	if _session_kind == KIND_SPLASH or _session_kind == KIND_BENCHMARK:
		return true
	if _session_kind == KIND_TUTORIAL and _base_name == &"dry_dock":
		var editor := get_node_or_null(^"/root/Editor")
		return editor != null and bool(editor.call(&"is_open"))
	return false


func _on_session_state(state: int) -> void:
	if not routing:
		return
	if state == SESSION_LOADING or state == SESSION_IN_SORTIE or state == SESSION_REPLAY:
		_session_kind = _session_info_kind()
	if _owned_elsewhere():
		if state == SESSION_IDLE or state == SESSION_FAILED:
			_session_kind = -1
		return
	match state:
		SESSION_LOADING:
			_failure_shown = false
			_failure_text = {}
			_world_warm = false
			show_loading(&"shell.loading.sortie", 0.0)
			if _loading != null:
				_loading.call(&"follow_session", true)
			_enter_world(&"replay" if _session_kind == KIND_REPLAY else &"sortie")
		SESSION_IN_SORTIE:
			_enter_world(&"sortie")
			_lift_cover_when_warm()
		SESSION_REPLAY:
			_enter_world(&"replay")
			_lift_cover_when_warm()
		SESSION_LOBBY:
			# A lobby can fail without passing through loading (a join that drops), so a lobby starts a
			# fresh failure report too.
			_failure_shown = false
			_failure_text = {}
			hide_loading()
			change(&"lobby")
		SESSION_ENDED:
			hide_loading()
			if not _campaign_active() and not is_open(&"sortie_end"):
				push(&"sortie_end")
		SESSION_FAILED:
			hide_loading()
			_show_failure.call_deferred()
		SESSION_IDLE:
			hide_loading()
			_session_kind = -1
			_failure_shown = false
			if _base_name in SESSION_BASES and not _campaign_active():
				change(&"main_menu")


func _on_session_failed(text: Dictionary) -> void:
	if _owned_elsewhere():
		return
	_failure_text = text
	if routing:
		_show_failure.call_deferred()


func _show_failure() -> void:
	if _failure_shown:
		return
	_failure_shown = true
	hide_loading()
	var back_to := _return_to
	if _campaign_active():
		back_to = &"chart"
	if _base_name in SESSION_BASES or _base_name == &"":
		change(back_to if back_to != &"" else &"main_menu")
	var body: Variant = _failure_text if not _failure_text.is_empty() else &"shell.session.failed_body"
	_failure_text = {}
	open_message(&"shell.session.failed_title", body)


func _enter_world(screen: StringName) -> void:
	if _base_name == screen:
		return
	change(screen)
	var view := world_view()
	if view != null and view.has_signal(&"warmed_up"):
		view.connect(&"warmed_up", _on_world_warm, CONNECT_ONE_SHOT)


func _on_world_warm() -> void:
	_world_warm = true
	if _session_live():
		hide_loading()


func _session_live() -> bool:
	var session := get_node_or_null(^"/root/Session")
	if session == null:
		return false
	var s := int(session.call(&"state"))
	return s == SESSION_IN_SORTIE or s == SESSION_REPLAY


func _lift_cover_when_warm() -> void:
	if _world_warm:
		hide_loading()
		return
	_warm_timer = get_tree().create_timer(WARM_UP_LIMIT_S, true, false, true)
	_warm_timer.timeout.connect(_on_warm_limit.bind(_warm_timer))


func _on_warm_limit(timer: SceneTreeTimer) -> void:
	if timer != _warm_timer or _world_warm or not is_loading():
		return
	push_warning("Screens: the world did not report warmed_up within %d s; lifting the cover" % int(WARM_UP_LIMIT_S))
	hide_loading()


func _on_campaign_phase(phase: int) -> void:
	if not routing:
		return
	match phase:
		PHASE_CHART, PHASE_ADVANCING, PHASE_OVER:
			change(&"chart")
		PHASE_BRIEFING:
			change(&"briefing")
		PHASE_DRY_DOCK:
			# Campaign.open_dry_dock leaves the campaign's dock context for Editor.open; the dock
			# screen does not open the editor itself, so open it here unless something already did.
			var editor := get_node_or_null(^"/root/Editor")
			if editor != null and not bool(editor.call(&"is_open")):
				open_dry_dock(DOCK_CAMPAIGN)
			else:
				change(&"dry_dock")
		PHASE_AFTER_ACTION:
			change(&"after_action")
		PHASE_NONE:
			if _base_name in CAMPAIGN_BASES:
				change(&"main_menu")
