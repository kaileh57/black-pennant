extends SceneTree
## Headless checks of the dry-dock screen's logic (units ui-editor-shell, ui-editor-tray, ui-editor-hull).
## Run from game/:
##   godot --headless --path . -s res://test/unit/ui_editor_test.gd
## Opens the 22 m fast attack craft in a sandbox dock, shows res://ui/editor/dry_dock.tscn and checks:
## showing every panel changes nothing in the design (a field whose range changes must not report an edit);
## the tray lists kits and parts with no filter, parts only under a category, and its empty state; a hull
## field commits through set_handle as one step that undo reverts; generated part names; the context menu
## on a part; the clamped colour picker (a saturated pick is stored clamped, Cancel restores the entry); and
## that a cancelled stroke closes its undo group. Exits 1 on any failure.

## Loaded at run time: the screens' scripts name autoloads, which a -s script's constants cannot see yet.
const DOCK_PATH := "res://ui/editor/dry_dock.tscn"
const INSPECTOR_PATH := "res://ui/editor/part_inspector.gd"

var editor: Node
var failures := 0


func _initialize() -> void:
	_run.call_deferred()


func check(ok: bool, what: String) -> void:
	if not ok:
		failures += 1
		push_error("ui_editor_test: " + what)


func frames(n: int) -> void:
	for i in n:
		await process_frame


func _run() -> void:
	editor = root.get_node("Editor")
	var issues: Array = editor.call(&"open", Bp.DOCK_SANDBOX, {"blueprint": "starters/fac22.bp.ron"})
	for i: Dictionary in issues:
		check(not bool(i.get("fatal", false)), "open: " + str(issues))
	var dock := (load(DOCK_PATH) as PackedScene).instantiate() as Control
	root.add_child(dock)
	await frames(30)
	var rev0 := int(editor.call(&"rev"))

	# Every panel shown, every station picked: nothing may change the design.
	var tabs := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs") as TabContainer
	var hull := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Hull")
	var paint := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Paint")
	for t in tabs.get_tab_count():
		tabs.current_tab = t
		await frames(3)
	for page in 3:
		hull.call(&"_on_mode", page)
		await frames(2)
	hull.call(&"_on_mode", 0)
	var stations: Array = Dictionary(editor.call(&"hull")).get("stations", [])
	for s in stations.size():
		hull.call(&"focus_station", s)
		await frames(1)
	for page in 3:
		paint.call(&"_on_mode", page)
		await frames(2)
	check(not bool(editor.call(&"is_dirty")), "showing the panels dirtied the design")
	check(Dictionary(editor.call(&"undo_state")).get("depth", 0) == 0, "showing the panels made undo steps")

	# Tray: kits and parts unfiltered; a category lists parts only; a search with no match shows the empty state.
	tabs.current_tab = 0
	await frames(2)
	var tray := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray")
	var rows: Dictionary = tray.get(&"_rows")
	var kits := 0
	var parts := 0
	for key: String in rows:
		if key.begins_with("kit:"):
			kits += 1
		else:
			parts += 1
	check(kits > 0 and parts > 0, "tray lists kits %d and parts %d" % [kits, parts])
	var category := tray.get_node(^"Filters/Category") as OptionButton
	category.select(category.get_item_index(Bp.CATEGORY_GUN))
	category.item_selected.emit(category.selected)
	await frames(1)
	rows = tray.get(&"_rows")
	var only_parts := not rows.is_empty()
	for key: String in rows:
		if not key.begins_with("part:"):
			only_parts = false
	check(only_parts, "a category lists single parts only")
	var search := tray.get_node(^"Search") as LineEdit
	search.text = "zzzz no such part"
	search.text_changed.emit(search.text)
	await frames(1)
	check((tray.get_node(^"Empty") as Control).visible and not (tray.get_node(^"Scroll") as Control).visible, "empty search shows the empty state")
	tray.call(&"_clear_filters")
	await frames(1)
	check(not Dictionary(tray.get(&"_rows")).is_empty(), "clearing the filters lists items again")

	# A hull field commits through set_handle; undo reverts it.
	tabs.current_tab = 1
	hull.call(&"_on_mode", 0)
	await frames(2)
	var fields: Dictionary = hull.get(&"_fields")
	var beam: SpinBox = fields.get("beam:*")
	check(beam != null, "beam field exists")
	if beam != null:
		var before := beam.value
		var want := snappedf(before + 0.3, beam.step)
		beam.value = want
		await frames(2)
		var after := float(Dictionary(Array(editor.call(&"handles")).filter(func(h: Dictionary) -> bool: return h["id"] == "beam" and int(h["station"]) == int(hull.call(&"current_station")))[0])["value"])
		check(absf(after - beam.value) < 0.0001, "beam field shows the applied value %f vs %f" % [beam.value, after])
		check(bool(editor.call(&"is_dirty")), "a field edit marks the design changed")
		editor.call(&"undo")
		await frames(2)
		check(absf(beam.value - before) < 0.0001, "undo puts the field back (%f vs %f)" % [beam.value, before])

	# Generated part names follow category order, never the fallback "part N".
	var placed: Array = editor.call(&"parts")
	check(not placed.is_empty(), "the starter has parts")
	if not placed.is_empty():
		var name := String((load(INSPECTOR_PATH) as GDScript).call(&"part_name", int(placed[0]["slot"])))
		check(not name.is_empty() and not name.begins_with("part "), "generated name " + name)

	# The context menu on a part offers Why? and the part actions.
	var context := dock.get_node(^"Context")
	if not placed.is_empty():
		context.call(&"open_dock", Vector2(400, 300), {"kind": &"part", "slot": int(placed[0]["slot"])}, false)
		var menu := context.get_node(^"Menu") as PopupMenu
		check(menu.item_count >= 8, "context menu items %d" % menu.item_count)
		check(menu.get_item_index(0) >= 0, "context menu has Why?")
		context.call(&"close")

	# The picker stores the clamped colour, and Cancel restores the empty entry.
	tabs.current_tab = 2
	paint.call(&"_on_mode", 1)
	await frames(2)
	var depth0 := int(Dictionary(editor.call(&"undo_state")).get("depth", 0))
	paint.set(&"_paint_index", 9)
	paint.call(&"_open_picker")
	var picker := paint.get_node(^"Picker/Margin/Column/Colour") as ColorPicker
	var red := picker.color
	red.r = 1.0
	red.g = 0.0
	red.b = 0.0
	picker.color = red
	paint.call(&"_store_pick")
	var stored: Dictionary = Array(editor.call(&"palette"))[9]
	var rgb: Vector3 = stored.get("rgb", Vector3.ONE)
	check(not bool(stored.get("empty", true)), "the pick was stored")
	check(rgb.x < 0.6 and rgb.y > 0.05 and rgb.z > 0.05, "a saturated red is clamped toward grey: " + str(rgb))
	check(String((paint.get_node(^"Picker/Margin/Column/Result/Note") as Label).text) == tr(&"editor_hull.picker.clamped_hull"), "the note says it was clamped")
	paint.call(&"_close_picker", true)
	await frames(2)
	check(bool(Array(editor.call(&"palette"))[9].get("empty", false)), "cancel restores the empty entry")
	check(int(Dictionary(editor.call(&"undo_state")).get("depth", 0)) == depth0, "a cancelled pick leaves no undo step")

	# A stroke cancelled with Esc closes its group and changes nothing.
	var depth1 := int(Dictionary(editor.call(&"undo_state")).get("depth", 0))
	var rev1 := int(editor.call(&"rev"))
	dock.call(&"_begin_stroke", Bp.TOOL_PAINT, Vector2(10, 10))
	dock.call(&"_end_drag", true)
	editor.call(&"set_design_name", "Group check")
	check(int(Dictionary(editor.call(&"undo_state")).get("depth", 0)) == depth1 + 1, "the next edit is its own undo step after a cancelled stroke")
	check(int(editor.call(&"rev")) > rev1, "revision moved on")
	check(rev0 > 0, "opened with a revision")

	dock.queue_free()
	editor.call(&"close")
	await frames(2)
	print("ui_editor_test: %s" % ("ok" if failures == 0 else "%d failures" % failures))
	quit(1 if failures > 0 else 0)
