extends Control
## Main menu: campaign, skirmish, sandbox, multiplayer, dry dock, tutorial, replays, benchmark, settings, credits, quit.
## DESIGN.md L1578-1614, L1697. Unit: ui-menu.
## Reads and calls: Screens, Campaign.suspend_slot, Tutorial.was_finished, AudioFeed.set_menu_theme.
##
## One column, no section labels (L1578 bans them on menus): groups are separated by space only.
## The first button is the one thing the player most likely wants, drawn taller, with one support
## line under it:
## 1. a suspended sortie: Resume sortie (L826; the suspend save is deleted once the sortie runs);
## 2. a first launch (tutorial never finished, no campaign yet): Start the tutorial (L254);
## 3. the newest loadable campaign: Continue <name>;
## 4. otherwise: Start a campaign.
## Whatever is not the first button stays in the groups below, so nothing moves out of reach. The one
## exception: with no campaign saves at all and Start a campaign first, the campaign row (the saves list,
## which would only offer Start a campaign again) is hidden, so the first button is never repeated below.
## Every label is an effect verb (L1697 rule 3). Esc on the menu moves focus to Quit; it never quits.
##
## Layout: the menu is one opaque column docked on the left (L1594: no glass), over the dusk sea of
## res://world/menu_stage.tscn, which fills the rest of the screen. The sea stops drawing while a
## pushed screen covers the menu.
##
## Navigation: the setup screens (campaign, skirmish, sandbox, multiplayer, benchmark) replace the
## menu as the base, so a start that fails lands back on them (`Screens` returns to the last base);
## settings, credits and replays are pushed over it and give focus back on close. Loading and
## starting only report problems: session and campaign state move the screens (`Screens` routing).

const Kit := preload("res://ui/screens/menu_kit.gd")

## `Bp` values used here, kept so the script parses before the extension loads.
const DOCK_SANDBOX := 0
const NONE := -1

enum Primary { RESUME, TUTORIAL, CONTINUE, START }

@onready var _column: VBoxContainer = $Scroll/Margin/Column
@onready var _primary: Button = $Scroll/Margin/Column/Primary/PrimaryButton
@onready var _primary_detail: Label = $Scroll/Margin/Column/Primary/PrimaryDetail
@onready var _campaign: Button = $Scroll/Margin/Column/Play/Campaign
@onready var _skirmish: Button = $Scroll/Margin/Column/Play/Skirmish
@onready var _sandbox: Button = $Scroll/Margin/Column/Play/Sandbox
@onready var _multiplayer: Button = $Scroll/Margin/Column/Play/Multiplayer
@onready var _dry_dock: Button = $Scroll/Margin/Column/Build/DryDock
@onready var _tutorial: Button = $Scroll/Margin/Column/Build/Tutorial
@onready var _replays: Button = $Scroll/Margin/Column/More/Replays
@onready var _benchmark: Button = $Scroll/Margin/Column/More/Benchmark
@onready var _settings: Button = $Scroll/Margin/Column/More/Settings
@onready var _credits: Button = $Scroll/Margin/Column/More/Credits
@onready var _quit: Button = $Scroll/Margin/Column/Quit
@onready var _version: Label = $Scroll/Margin/Column/Version
@onready var _stage: Node = $Stage

## The menu button that last opened a setup screen in place of the menu, by node name, so coming
## back from it puts focus where the player left (the menu is rebuilt each time).
static var _return_focus: StringName = &""

var _mode := Primary.START
## The slot the first button acts on (suspend or newest campaign), or empty.
var _primary_slot: Dictionary = {}
var _tutorial_finished := false
## No campaign save of any kind exists (`apply_state`).
var _no_saves := false

var _theme_started := false

func _ready() -> void:
	Ui.set_margins($Scroll/Margin, 6)
	Ui.set_separation(_column, 5)
	for box: Node in [$Scroll/Margin/Column/Primary, $Scroll/Margin/Column/Play, $Scroll/Margin/Column/Build, $Scroll/Margin/Column/More]:
		Ui.set_separation(box as Control, 1)
	_primary_detail.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_primary.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_version.auto_translate_mode = Node.AUTO_TRANSLATE_MODE_DISABLED
	_primary_detail.text = ""
	_version.text = ""
	_primary.pressed.connect(_on_primary)
	_campaign.pressed.connect(_go.bind(&"campaign_slots", _campaign))
	_skirmish.pressed.connect(_go.bind(&"skirmish_setup", _skirmish))
	_sandbox.pressed.connect(_go.bind(&"sandbox_setup", _sandbox))
	_multiplayer.pressed.connect(_go.bind(&"multiplayer", _multiplayer))
	_dry_dock.pressed.connect(_open_dry_dock)
	_tutorial.pressed.connect(_start_tutorial)
	_replays.pressed.connect(func() -> void: Screens.push(&"replay_browser"))
	_benchmark.pressed.connect(_go.bind(&"benchmark", _benchmark))
	_settings.pressed.connect(func() -> void: Screens.push(&"settings"))
	_credits.pressed.connect(func() -> void: Screens.push(&"credits"))
	_quit.pressed.connect(_ask_quit)
	accessibility_name = tr(&"menu.title")
	Screens.screen_changed.connect(_on_screen_changed)
	_render()


func _exit_tree() -> void:
	Kit.leave_menu_theme(_theme_started)


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_render()


func setup(_args: Dictionary) -> void:
	refresh()
	_theme_started = true
	Kit.start_menu_theme()


func default_focus() -> Control:
	var back := _column.find_child(String(_return_focus), true, false) as Control if _return_focus != &"" else null
	_return_focus = &""
	if back != null and back.is_visible_in_tree():
		return back
	return _primary


func _go(screen: StringName, from: Button) -> void:
	_return_focus = from.name
	Kit.go(screen)


func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"ui_cancel", false, true) and Screens.top() == self:
		get_viewport().set_input_as_handled()
		Ui.focus(_quit)


func _on_screen_changed(name: StringName) -> void:
	if is_instance_valid(_stage) and _stage.has_method(&"set_covered"):
		_stage.call(&"set_covered", Screens.top() != self)
	# Back on top after a pushed screen closed (a replay was deleted, a setting changed): re-read.
	if name == &"main_menu" and Screens.top() == self:
		refresh()


## Read what the first button should offer from the facades, then draw.
func refresh() -> void:
	var suspend := Kit.as_dict(Kit.call_on(&"Campaign", &"suspend_slot"))
	var slots := Kit.dicts(Kit.call_on(&"Campaign", &"slots"))
	var finished: Variant = Kit.call_on(&"Tutorial", &"was_finished")
	apply_state(suspend, slots, finished == true)
	var info := Kit.as_dict(Kit.call_on(&"App", &"build_info"))
	var version := Kit.get_str(info, "version")
	_version.text = tr(&"menu.version").format([version, Kit.get_str(info, "build_id", "local")]) if not version.is_empty() else ""


## Pick the first button from plain data (also the test entry point): `suspend` is a SaveSlotDict
## or empty, `slots` the SaveSlotDicts newest first, `tutorial_finished` from Tutorial.
func apply_state(suspend: Dictionary, slots: Array[Dictionary], tutorial_finished: bool) -> void:
	_primary_slot = {}
	var newest: Dictionary = {}
	for s: Dictionary in slots:
		if Kit.get_bool(s, "loadable", true) and not Kit.get_bool(s, "is_suspend"):
			newest = s
			break
	if not suspend.is_empty() and Kit.get_bool(suspend, "loadable", true):
		_mode = Primary.RESUME
		_primary_slot = suspend
	elif not tutorial_finished and slots.is_empty():
		_mode = Primary.TUTORIAL
	elif not newest.is_empty():
		_mode = Primary.CONTINUE
		_primary_slot = newest
	else:
		_mode = Primary.START
	_tutorial_finished = tutorial_finished
	_no_saves = slots.is_empty()
	_render()


func _render() -> void:
	match _mode:
		Primary.RESUME:
			_primary.text = tr(&"menu.resume")
			_primary_detail.text = tr(&"menu.resume.detail").format([_slot_summary(_primary_slot)])
		Primary.TUTORIAL:
			_primary.text = tr(&"menu.tutorial.start")
			_primary_detail.text = tr(&"menu.tutorial.detail")
		Primary.CONTINUE:
			_primary.text = tr(&"menu.continue").format([Kit.get_str(_primary_slot, "slot_name")])
			_primary_detail.text = _slot_summary(_primary_slot)
		Primary.START:
			_primary.text = tr(&"menu.campaign.start")
			_primary_detail.text = tr(&"menu.campaign.start.detail")
	_primary_detail.visible = not _primary_detail.text.is_empty()
	_primary.accessibility_description = _primary_detail.text
	# The tutorial button stays below unless the first button already offers it.
	_tutorial.visible = _mode != Primary.TUTORIAL
	_tutorial.text = "menu.tutorial.again" if _tutorial_finished else "menu.tutorial.start"
	_campaign.visible = not (_mode == Primary.START and _no_saves)
	_link_focus()


func _slot_summary(slot: Dictionary) -> String:
	if slot.is_empty():
		return ""
	var act := Kit.get_int(slot, "act", 0)
	var act_word := String(Kit.call_on(&"Tx", &"word", [&"act", act])) if Kit.autoload(&"Tx") != null else ""
	if act_word.is_empty():
		act_word = tr(StringName("units.word.act.%d" % (clampi(act, 0, 3) + 1)))
	var played := Kit.get_int(slot, "played_s", 0)
	var played_text := String(Kit.call_on(&"Tx", &"duration", [float(played)])) if Kit.autoload(&"Tx") != null else ""
	var line := tr(&"menu.slot.summary").format([act_word, Kit.get_int(slot, "day", 1), played_text])
	var saved := Kit.local_time(Kit.get_int(slot, "modified_unix", 0))
	if not saved.is_empty():
		line += " " + tr(&"menu.slot.saved").format([saved])
	if Kit.get_bool(slot, "ironman"):
		line += " " + tr(&"menu.slot.ironman")
	return line


func _link_focus() -> void:
	Kit.chain_vertical([_primary, _campaign, _skirmish, _sandbox, _multiplayer, _dry_dock, _tutorial,
		_replays, _benchmark, _settings, _credits, _quit])
	for b: Button in [_primary, _campaign, _skirmish, _sandbox, _multiplayer, _dry_dock, _tutorial, _replays, _benchmark, _settings, _credits, _quit]:
		b.focus_neighbor_left = b.get_path_to(b)
		b.focus_neighbor_right = b.get_path_to(b)


# --- actions -------------------------------------------------------------------------------------

func _on_primary() -> void:
	match _mode:
		Primary.RESUME:
			_resume()
		Primary.TUTORIAL:
			_start_tutorial()
		Primary.CONTINUE:
			_continue(Kit.get_str(_primary_slot, "path"))
		Primary.START:
			Kit.go(&"campaign_new")


func _resume() -> void:
	if not await Kit.resume_suspend():
		refresh()


func _continue(path: String) -> void:
	if not await Kit.load_campaign(path):
		refresh()


func _start_tutorial() -> void:
	var ok: Variant = Kit.call_on(&"Tutorial", &"start", [NONE])
	if ok != true:
		Screens.open_message(&"menu.tutorial.failed_title", &"menu.tutorial.failed_body")
		return
	# The tutorial opens the dry dock on its hull; the dock screen shows it.
	Screens.change(&"dry_dock")


func _open_dry_dock() -> void:
	var issues: Variant = Kit.call_on(&"Screens", &"open_dry_dock", [DOCK_SANDBOX, {}])
	if not issues is Array:
		Screens.open_message(&"menu.dry_dock.failed_title", &"menu.dry_dock.failed_body")
		return
	var fatal := false
	for issue: Dictionary in Kit.dicts(issues):
		fatal = fatal or Kit.get_bool(issue, "fatal")
	if fatal:
		Screens.open_message(&"menu.dry_dock.failed_title", Kit.problems_body(Kit.problem_texts(issues, &"menu.dry_dock.failed_body")))


func _ask_quit() -> void:
	var dirty: Variant = Kit.call_on(&"Editor", &"is_dirty")
	if dirty != true:
		get_tree().quit()
		return
	var dialog := Screens.open_dialog(&"menu.quit.unsaved_title", &"menu.quit.unsaved_body", [
		{"id": &"quit", "label": &"menu.quit.confirm"},
		{"id": &"stay", "label": &"menu.quit.stay"},
	], &"stay", &"stay")
	if dialog != null:
		dialog.connect(&"chosen", _on_quit_chosen, CONNECT_ONE_SHOT)


func _on_quit_chosen(id: StringName) -> void:
	if id == &"quit":
		get_tree().quit()
