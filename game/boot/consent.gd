extends Control
## First-run crash-report consent: one question, equal-weight "Send" and "Don't send", nothing preselected, the list of what a report holds.
## DESIGN.md L1334. Unit: ui-boot.
## Reads and calls: Settings.set_value("privacy.crash_reports"), App.crash_reports_init.
##
## Boot shows this screen while `privacy.crash_reports` is -1 (not asked) and awaits `decided`.
## Nothing is written or sent before the choice (L1334): the Sentry SDKs start only in
## `start_reporting()`, after Send.
##
## Equal weight: both buttons share one look, one size and one row. Nothing is preselected: the
## screen opens with no focus at all, so Enter or the gamepad's accept button chooses nothing. The
## first direction pressed picks a button by where it sits: Left, Up or Down the left one (Send),
## Right the right one (Don't send); after that Left and Right move between the two. Esc chooses
## nothing either. What a report holds is plain text in two columns, not a list to move through.
##
## `start_reporting()` and `stop_reporting()` are static so Settings > Privacy (ui-settings) runs the
## same code when the player changes the toggle there, which takes effect at once (L1334).

signal decided(send: bool)

## What a report holds, in the order L1334 lists it.
const REPORT_ITEMS: Array[StringName] = [
	&"boot.consent.item.stack_trace",
	&"boot.consent.item.minidump",
	&"boot.consent.item.os",
	&"boot.consent.item.gpu",
	&"boot.consent.item.driver",
	&"boot.consent.item.renderer",
	&"boot.consent.item.fallbacks",
	&"boot.consent.item.tier",
	&"boot.consent.item.build",
	&"boot.consent.item.role",
	&"boot.consent.item.pipelines",
	&"boot.consent.item.install_id",
]
## The Godot SDK's crash database and installation id live here; withdrawing consent deletes it.
const SENTRY_DIR := "user://sentry"
const DSN_SETTING := "sentry/options/dsn"

@onready var _contents: GridContainer = $Scroll/Center/Margin/Column/Contents
@onready var _send: Button = $Scroll/Center/Margin/Column/Buttons/Send
@onready var _dont_send: Button = $Scroll/Center/Margin/Column/Buttons/DontSend

var _decided := false


func _ready() -> void:
	Ui.set_margins($Scroll/Center/Margin, 6)
	Ui.set_separation($Scroll/Center/Margin/Column, 4)
	Ui.set_separation($Scroll/Center/Margin/Column/Buttons, 4)
	_contents.add_theme_constant_override(&"h_separation", Ui.spacing(5))
	_contents.add_theme_constant_override(&"v_separation", Ui.spacing(0))
	_fill_contents()
	_send.pressed.connect(_choose.bind(true))
	_dont_send.pressed.connect(_choose.bind(false))


func _notification(what: int) -> void:
	if what == NOTIFICATION_TRANSLATION_CHANGED and is_node_ready():
		_fill_contents()


## Nothing takes focus when the screen opens, so neither button starts chosen. The screen root
## takes no focus, so Screens' own first-focus fallback does nothing here either.
func default_focus() -> Control:
	return self


## The first direction pressed with nothing focused picks the button on that side.
func _unhandled_input(event: InputEvent) -> void:
	if get_viewport().gui_get_focus_owner() != null:
		return
	var target: Button = null
	if event.is_action_pressed(&"ui_right", false, true):
		target = _dont_send
	elif event.is_action_pressed(&"ui_left", false, true) or event.is_action_pressed(&"ui_up", false, true) or event.is_action_pressed(&"ui_down", false, true):
		target = _send
	if target != null:
		get_viewport().set_input_as_handled()
		Ui.focus(target)


func _fill_contents() -> void:
	_contents.accessibility_name = tr(&"boot.consent.contents")
	for child: Node in _contents.get_children():
		_contents.remove_child(child)
		child.queue_free()
	for key: StringName in REPORT_ITEMS:
		var item := Label.new()
		item.theme_type_variation = &"Body"
		item.text = key
		_contents.add_child(item)


func _choose(send: bool) -> void:
	if _decided:
		return
	_decided = true
	var settings := _autoload(&"Settings")
	if settings != null:
		settings.call(&"set_value", &"privacy.crash_reports", 1 if send else 0)
	if send:
		start_reporting()
	decided.emit(send)


## Start both Sentry SDKs with one DSN, release, dist and environment (L1340), then set the tags.
## Safe to call when no DSN is configured or the Godot SDK is not installed: each part that is
## missing is skipped and the rest still runs.
static func start_reporting() -> void:
	var app := _autoload(&"App")
	if app == null:
		return
	var info: Dictionary = app.call(&"build_info")
	var dsn := String(ProjectSettings.get_setting(DSN_SETTING, ""))
	var release := "black-pennant@%s" % String(info.get("version", ""))
	var dist := String(info.get("build_id", ""))
	var environment := "debug" if bool(info.get("debug", false)) else "release"
	if Engine.has_singleton(&"SentrySDK"):
		var sdk := Engine.get_singleton(&"SentrySDK")
		if sdk.has_method(&"init"):
			sdk.call(&"init")
	app.call(&"crash_reports_init", dsn, release, dist, environment)
	var report: Variant = app.call(&"startup_report")
	if report is Dictionary:
		var r := report as Dictionary
		app.call(&"crash_reports_set_tag", "renderer", String(r.get("rendering_method", "")))
		app.call(&"crash_reports_set_tag", "driver", String(r.get("driver", "")))
	app.call(&"crash_reports_set_tag", "build_id", dist)
	var quality := _autoload(&"Quality")
	if quality != null:
		var profile: Variant = quality.call(&"profile")
		if profile is Dictionary:
			app.call(&"crash_reports_set_tag", "tier", str(int((profile as Dictionary).get("tier", -1))))


## Withdraw consent: close both SDKs and delete the crash database and installation id (L1334).
static func stop_reporting() -> void:
	var app := _autoload(&"App")
	if app != null:
		app.call(&"crash_reports_close")
	if Engine.has_singleton(&"SentrySDK"):
		var sdk := Engine.get_singleton(&"SentrySDK")
		if sdk.has_method(&"close"):
			sdk.call(&"close")
	_delete_tree(SENTRY_DIR)


static func _delete_tree(path: String) -> void:
	var dir := DirAccess.open(path)
	if dir == null:
		return
	dir.include_hidden = true
	for sub: String in dir.get_directories():
		_delete_tree(path.path_join(sub))
	for file: String in dir.get_files():
		DirAccess.remove_absolute(path.path_join(file))
	DirAccess.remove_absolute(path)


static func _autoload(autoload_name: StringName) -> Node:
	var tree := Engine.get_main_loop() as SceneTree
	if tree == null or tree.root == null:
		return null
	return tree.root.get_node_or_null(NodePath(String(autoload_name)))
