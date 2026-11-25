extends Node
## Plays the dry dock the way a player does, from the real boot scene, and saves a viewport capture at
## every step for review against DESIGN.md "The editor" and "Interface and visual design". Input is real:
## mouse, key and trackpad events go through `Input.parse_input_event`, so they reach the screens the same
## way a player's do. Run from the repository root with a throwaway HOME (the run writes user:// files):
##   HOME=/tmp/bp-play godot --path game --resolution 1920x1080 res://test/ui/dock_play.tscn -- \
##       --plan=<plan> --out=<folder>
## Plans: build (an empty slip to a fitted, saved ship), overlays (every overlay and the read tabs),
## starter (modify a starter hull), share (share string export and import), modes (control graph and
## scripts), camera (mouse and trackpad camera). Prints each step and quits; exits 1 on a failed step.
## Headless, and when instanced by check_project, it does nothing.

var _plan := "build"
var _out := "user://captures/play"
var _step := 0
var _failed := false
var _scale := 1.0


func _ready() -> void:
	if DisplayServer.get_name() == "headless" or get_tree().current_scene != self:
		return
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--plan="):
			_plan = arg.trim_prefix("--plan=")
		elif arg.begins_with("--out="):
			_out = arg.trim_prefix("--out=")
		elif arg.begins_with("--scale="):
			_scale = float(arg.trim_prefix("--scale="))
	DirAccess.make_dir_recursive_absolute(_out)
	# The driver outlives the scene change to the boot scene.
	var driver := Node.new()
	driver.name = &"DockPlayDriver"
	driver.set_script(get_script())
	driver.set_meta(&"plan", _plan)
	driver.set_meta(&"out", _out)
	driver.set_meta(&"scale", _scale)
	driver.process_mode = Node.PROCESS_MODE_ALWAYS
	get_tree().root.add_child.call_deferred(driver)
	get_tree().change_scene_to_file.call_deferred("res://boot/boot.tscn")


func _enter_tree() -> void:
	if has_meta(&"plan"):
		_plan = String(get_meta(&"plan"))
		_out = String(get_meta(&"out"))
		_scale = float(get_meta(&"scale"))
		_run.call_deferred()


# --- the plans -------------------------------------------------------------------------------------

func _run() -> void:
	print("dock_play: plan ", _plan)
	await _to_main_menu()
	if _scale != 1.0:
		get_tree().root.content_scale_factor = _scale
		await _frames(10)
	match _plan:
		"build":
			await _plan_build()
		"overlays":
			await _plan_overlays()
		"starter":
			await _plan_starter()
		"share":
			await _plan_share()
		"modes":
			await _plan_modes()
		"camera":
			await _plan_camera()
		"menu":
			await _shot("main_menu")
		"look":
			await _plan_look()
		"scaled":
			await _plan_scaled()
		_:
			_fail("unknown plan " + _plan)
	print("dock_play: done, failed=", _failed)
	if Editor.is_open():
		Editor.close()
	get_tree().quit(1 if _failed else 0)


## Boot to the main menu: decline crash reports on the consent screen, wait out the first-launch checks.
func _to_main_menu() -> void:
	for i in 3600:
		await get_tree().process_frame
		var top := Screens.top()
		var name := String(Screens.top_name())
		if name == "main_menu" and top != null and (top as Control).is_visible_in_tree():
			await _wait(0.5)
			return
		var consent := get_tree().root.find_child("DontSend", true, false) as Button
		if consent != null and consent.is_visible_in_tree():
			await _wait(0.3)
			await _click(consent)
			await _wait(0.5)
	_fail("never reached the main menu")


func _open_dock_from_menu() -> Control:
	var menu := Screens.top()
	var button := menu.find_child("DryDock", true, false) as Button
	if button == null:
		_fail("no Dry dock button")
		return null
	await _scroll_into_view(button)
	await _click(button)
	return await _wait_dock()


func _wait_dock() -> Control:
	for i in 600:
		await get_tree().process_frame
		if Screens.top_name() == &"dry_dock" and Editor.is_open():
			var dock := Screens.top() as Control
			var empty := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Empty") as Control
			if not empty.visible:
				await _wait(0.6)
				return dock
	_fail("the dock never showed a design")
	return Screens.top() as Control


func _plan_build() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	await _shot("dock_opened")
	_log_state("opened")
	# File > New design > Empty hull, by keyboard in the real popups.
	var file := dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/File") as MenuButton
	await _click(file)
	await _wait(0.3)
	await _shot("file_menu")
	var popup := file.get_popup()
	_check(popup.visible, "the File menu opens")
	await _menu_focus(popup, popup.get_item_index(4))
	await _key(KEY_RIGHT)
	await _wait(0.4)
	await _shot("new_submenu")
	await _key(KEY_ENTER)
	await _wait(0.5)
	await _answer_dialog(&"discard")
	await _wait(1.5)
	await _shot("empty_hull")
	_log_state("empty")
	_check(Editor.parts().is_empty() and Editor.info().get("name", "x") == "", "File, New design, Empty hull opens an empty hull")
	# Hull tab: drag the beam handle.
	await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", 1)
	await _wait(0.6)
	await _shot("hull_tab")
	var beam := _handle(dock, "beam")
	_check(not beam.is_empty(), "the beam handle shows")
	if not beam.is_empty():
		var before := _handle_value("beam", int(beam.get("station", 0)))
		var at := _display_to_canvas(dock, Vector2(beam["screen"]))
		await _move(at)
		await _wait(0.3)
		await _shot("beam_hover")
		await _drag(at, at + Vector2(60, -30), 12)
		await _wait(0.8)
		await _shot("beam_dragged")
		var after := _handle_value("beam", int(beam.get("station", 0)))
		print("dock_play: beam ", before, " -> ", after)
		_check(after > before, "dragging the beam handle out widens the hull")
	# Parts tab: the four kits of the first ten minutes, by pointing at the deck.
	await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", 0)
	await _wait(0.5)
	await _shot("parts_tab")
	var tray := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray")
	var size: Vector3i = Editor.hull().get("size_cells", Vector3i.ZERO)
	var mid := size.z / 2
	var kits: Array = [["gun", ["kit.gun"], 0.72, 0], ["radar", ["kit.radar_mast"], 0.5, 0], ["waterjet", ["kit.engine.waterjet"], 0.16, 0], ["fuel", ["kit.fuel_tank"], 0.55, 2]]
	for k: Array in kits:
		var n := Editor.parts().size()
		await _tray_pick(tray, k[1])
		await _wait(0.3)
		var spot := _deck_point(dock, int(size.x * float(k[2])), mid + int(k[3]))
		await _move(spot)
		await _wait(0.5)
		await _shot("ghost_" + String(k[0]))
		_log_ghost(String(k[0]))
		var g: Variant = Editor.ghost()
		_check(g is Dictionary and bool((g as Dictionary).get("valid", false)), "the %s ghost is valid where the deck is pointed at" % k[0])
		await _click_at(spot)
		await _wait(0.6)
		_check(Editor.parts().size() > n, "a click places the %s (%d to %d parts)" % [k[0], n, Editor.parts().size()])
		await _key(KEY_ESCAPE)
		await _wait(0.3)
	await _shot("kits_placed")
	_log_state("kits")
	await _wait(3.0)
	await _shot("notes")
	_log_notes()
	# Undo and redo.
	var count := Editor.parts().size()
	await _key(KEY_Z, false, true)
	await _wait(0.6)
	await _shot("undone")
	_check(Editor.parts().size() < count, "Cmd+Z takes the last kit off")
	await _key(KEY_Z, true, true)
	await _wait(0.6)
	await _shot("redone")
	_check(Editor.parts().size() == count, "Cmd+Shift+Z puts it back")
	# Select the gun, drag it aft, turn it, delete it, undo.
	var gun := _part_of("gun.")
	_check(not gun.is_empty(), "the gun is on the ship")
	if not gun.is_empty():
		var at := _part_point(dock, gun)
		await _click_at(at)
		await _wait(0.5)
		await _shot("selected")
		var sel: PackedInt32Array = Editor.selection().get("parts", PackedInt32Array())
		_check(sel.size() > 0, "a click on the gun selects it")
		var cell_before: Vector3i = gun.get("cell", Vector3i.ZERO)
		await _drag(at, at + Vector2(-50, 10), 10)
		await _wait(0.5)
		await _shot("moved")
		var moved := _part_by_slot(int(gun.get("slot", -1)))
		print("dock_play: gun cell ", cell_before, " -> ", moved.get("cell"))
		_check(moved.get("cell") != cell_before, "dragging the selected gun moves it")
		await _key(KEY_R)
		await _wait(0.4)
		await _shot("rotated")
		var n := Editor.parts().size()
		await _key(KEY_DELETE)
		await _wait(0.5)
		await _shot("deleted")
		_check(Editor.parts().size() < n, "Delete removes the selection")
		await _key(KEY_Z, false, true)
		await _wait(0.5)
		_check(Editor.parts().size() == n, "undo brings the deleted gun back")
	await _key(KEY_ESCAPE)
	# Paint tab.
	await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", 2)
	await _wait(0.5)
	await _shot("paint_tab")
	# Officers tab.
	await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", 3)
	await _wait(0.5)
	await _shot("officers_tab")
	# Name and save.
	var name_field := dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/Name") as LineEdit
	await _click(name_field)
	await _key(KEY_A, false, true)
	await _type("Kestrel")
	await _key(KEY_ENTER)
	await _wait(0.3)
	await _key(KEY_S, false, true)
	await _wait(1.0)
	await _shot("saved")
	var share := Editor.share_string()
	print("dock_play: saved path ", Editor.design_path(), " dirty ", Editor.is_dirty())
	_check(String(Editor.design_path()).begins_with("user://") and not Editor.is_dirty(), "Cmd+S saves to user://")
	# Leave and reopen from the menu.
	var leave := dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/Leave") as Button
	await _click(leave)
	await _wait(1.0)
	await _answer_dialog(&"discard")
	await _wait(1.0)
	await _shot("back_to_menu")
	dock = await _open_dock_from_menu()
	await _shot("reopened")
	_log_state("reopened")
	_check(Editor.share_string() == share, "the reopened design is the saved one")


func _check(ok: bool, what: String) -> void:
	print("dock_play: ", "ok   " if ok else "FAIL ", what)
	if not ok:
		_fail(what)


func _handle_value(id: String, station: int) -> float:
	for h: Dictionary in Editor.handles():
		if String(h.get("id", "")) == id and int(h.get("station", -1)) == station:
			return float(h.get("value", 0.0))
	return NAN


func _part_of(prefix: String) -> Dictionary:
	for p: Dictionary in Editor.parts():
		if String(p.get("part", "")).begins_with(prefix):
			return p
	return {}


func _part_by_slot(slot: int) -> Dictionary:
	for p: Dictionary in Editor.parts():
		if int(p.get("slot", -1)) == slot:
			return p
	return {}


## Canvas position of a world point in the dock's 3D view.
func _world_point(dock: Control, world: Vector3) -> Vector2:
	var cam := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport/DockStage/Camera") as Camera3D
	return _display_to_canvas(dock, cam.unproject_position(world))


## A canvas point aiming at the weather deck over cell column (x, z): the projected top of the column, then
## the point near it whose `cell_at` lands closest to the column, highest first.
func _deck_point(dock: Control, x: int, z: int) -> Vector2:
	var size: Vector3i = Editor.hull().get("size_cells", Vector3i.ZERO)
	var view := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport/DockStage/View")
	var display := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Display") as Control
	var to_local := display.get_global_transform_with_canvas().affine_inverse()
	var guess := _world_point(dock, Vector3((x + 0.5) * 0.5, size.y * 0.5, (z + 0.5) * 0.5))
	var best := guess
	var best_score := INF
	var best_cell := Vector3i(-1, -1, -1)
	for dy in range(-240, 241, 8):
		for dx in range(-240, 241, 8):
			var at := guess + Vector2(dx, dy)
			var hit: Dictionary = view.call(&"pick", to_local * at)
			# Face 3 is +Y: the top of a cell, so the deck rather than a side.
			if StringName(hit.get("kind", &"")) != &"cell" or int(hit.get("face", -1)) != 3:
				continue
			var cell: Vector3i = hit.get("cell", Vector3i.ZERO)
			var score := absf(cell.x - x) + absf(cell.z - z) * 2.0
			if score < best_score:
				best_score = score
				best = at
				best_cell = cell
	print("dock_play: aiming at column ", Vector2i(x, z), ", hit ", best_cell, " score ", best_score)
	return best


## A canvas point where the view picks this part: tried over the cells around its corner cell.
func _part_point(dock: Control, part: Dictionary) -> Vector2:
	var view := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport/DockStage/View")
	var display := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Display") as Control
	var c := Vector3(Vector3i(part.get("cell", Vector3i.ZERO)))
	var slot := int(part.get("slot", -1))
	for dy: float in [2.5, 1.5, 0.5, 3.5]:
		for dx: float in [1.5, 0.5, 2.5]:
			for dz: float in [1.5, 0.5, 2.5]:
				var at := _world_point(dock, (c + Vector3(dx, dy, dz)) * 0.5)
				var local := display.get_global_transform_with_canvas().affine_inverse() * at
				var hit: Dictionary = view.call(&"pick", local)
				if StringName(hit.get("kind", &"")) == &"part" and int(hit.get("slot", -2)) == slot:
					return at
	print("dock_play: no screen point picks slot ", slot)
	return _world_point(dock, (c + Vector3(0.5, 0.5, 0.5)) * 0.5)


## The dock at a large UI scale: the top, then scrolled down to the build area, on each left tab.
func _plan_scaled() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var scroll := dock.get_node(^"Scroll") as ScrollContainer
	await _shot("scaled_top")
	_check(scroll.get_h_scroll_bar().visible == false, "the dock never scrolls sideways")
	var body := dock.get_node(^"Scroll/Frame/Body") as Control
	for i in 4:
		scroll.scroll_vertical = int(body.position.y)
		await _frames(3)
		await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", i)
		await _wait(0.6)
		await _shot("scaled_body_tab_%d" % i)
	var reports := dock.get_node(^"Scroll/Frame/Body/Centre/ViewBar/Margin/Row/Reports") as Button
	if reports.visible:
		await _click(reports)
		await _wait(0.6)
		await _shot("scaled_reports")


## The dock view itself: the starter and an empty hull from each camera preset, overlays off.
func _plan_look() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var view := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport/DockStage/View")
	view.call(&"set_overlays", 0)
	if OS.get_cmdline_user_args().has("--debug-draw"):
		var vp := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport") as SubViewport
		var stage := dock.get_node(^"Scroll/Frame/Body/Centre/Stage") as Control
		view.call(&"frame", &"port")
		await _wait(0.5)
		for i in 5:
			view.call(&"zoom", 1.25, Vector2(stage.size.x * 0.3, stage.size.y * 0.6))
		await _wait(0.5)
		for mode: int in [Viewport.DEBUG_DRAW_DISABLED, Viewport.DEBUG_DRAW_UNSHADED, Viewport.DEBUG_DRAW_NORMAL_BUFFER, Viewport.DEBUG_DRAW_WIREFRAME]:
			vp.debug_draw = mode as Viewport.DebugDraw
			await _wait(0.4)
			await _shot_stage(dock, "debug_%d" % mode)
		vp.debug_draw = Viewport.DEBUG_DRAW_DISABLED
		return
	for bp: String in ["", "empty"]:
		if bp == "empty":
			Editor.new_design("", 22.0)
			await _wait(1.5)
		for frame: StringName in [&"all", &"port", &"bow", &"top", &"below"]:
			view.call(&"frame", frame)
			await _wait(0.6)
			await _shot_stage(dock, "%s_%s" % [bp if bp != "" else "starter", frame])


func _shot_stage(dock: Control, name: String) -> void:
	await RenderingServer.frame_post_draw
	_step += 1
	var vp := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Viewport") as SubViewport
	var path := _out.path_join("%02d_%s.png" % [_step, name])
	vp.get_texture().get_image().save_png(path)
	print("dock_play: shot ", path)


func _plan_overlays() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var bar := dock.get_node(^"Scroll/Frame/Body/Centre/Overlays")
	await _shot("overlay_bar")
	var buttons: Array[Button] = []
	for b: Node in bar.find_children("*", "Button", true, false):
		if (b as Button).is_visible_in_tree() and (b as Button).toggle_mode:
			buttons.append(b as Button)
	print("dock_play: overlay toggles ", buttons.size())
	for b: Button in buttons:
		await _click(b)
		await _wait(1.0)
		await _shot("overlay_" + _slug(b.text if not b.text.is_empty() else b.tooltip_text))
		await _click(b)
		await _wait(0.3)
	for i in 3:
		await _tab(dock, "Scroll/Frame/Body/RightDock/Tabs", i)
		await _wait(0.8)
		await _shot("right_tab_%d" % i)


## Modify a starter hull: File, New design, the 40 m corvette; widen it, add a gun, save it as a copy.
func _plan_starter() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var file := dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/File") as MenuButton
	await _click(file)
	await _wait(0.3)
	var popup := file.get_popup()
	await _menu_focus(popup, popup.get_item_index(4))
	await _key(KEY_RIGHT)
	await _wait(0.4)
	var starters := popup.get_node(^"Starters") as PopupMenu
	var want := -1
	for i in starters.item_count:
		if starters.get_item_text(i).contains("40"):
			want = i
	_check(want > 0, "the New design menu lists the 40 m corvette")
	await _menu_focus(starters, want)
	await _shot("starters_menu")
	await _key(KEY_ENTER)
	await _wait(0.5)
	await _answer_dialog(&"discard")
	await _wait(2.0)
	await _shot("starter_opened")
	_log_state("starter")
	var info := Editor.info()
	_check(String(info.get("name", "")).length() > 0 and Editor.parts().size() > 10, "the corvette opens with its parts")
	_check(not String(Editor.design_path()).begins_with("user://"), "a starter opens as stock, not a user file")
	# Widen station 3 by its field in the Hull tab.
	await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", 1)
	await _wait(0.5)
	await _shot("starter_hull_tab")
	var n := Editor.parts().size()
	var tray := dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Parts/Tray")
	await _tab(dock, "Scroll/Frame/Body/LeftDock/Tabs", 0)
	await _wait(0.4)
	await _tray_pick(tray, ["kit.gun.40mm"])
	await _wait(0.3)
	var size: Vector3i = Editor.hull().get("size_cells", Vector3i.ZERO)
	var placed := false
	for f: float in [0.2, 0.15, 0.25, 0.1, 0.3]:
		var spot := _deck_point(dock, int(size.x * f), size.z / 2)
		await _move(spot)
		await _wait(0.4)
		var g: Variant = Editor.ghost()
		if g is Dictionary and bool((g as Dictionary).get("valid", false)):
			await _shot("starter_ghost")
			await _click_at(spot)
			await _wait(0.6)
			placed = Editor.parts().size() > n
			break
	_check(placed, "a 40 mm gun goes on the corvette's quarterdeck")
	await _key(KEY_ESCAPE)
	await _wait(0.5)
	await _shot("starter_modified")
	await _key(KEY_S, false, true)
	await _wait(1.0)
	await _shot("starter_saved")
	print("dock_play: saved starter copy ", Editor.design_path())
	_check(String(Editor.design_path()).begins_with("user://"), "saving a modified starter writes a user copy")


## Share: copy the design's share string, then import it back through File, Import a share string.
func _plan_share() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var original := Editor.share_string()
	var file := dock.get_node(^"Scroll/Frame/TopBar/Margin/Row/File") as MenuButton
	await _click(file)
	await _wait(0.3)
	var popup := file.get_popup()
	await _menu_focus(popup, popup.get_item_index(3))
	await _key(KEY_ENTER)
	await _wait(1.0)
	await _shot("share_copy")
	var top := Screens.top()
	_check(Screens.top_name() == &"share_dialog", "File, Share opens the share dialog")
	var primary := top.get(&"_primary") as Button
	await _click(primary)
	await _wait(0.5)
	await _shot("share_copied")
	var clip := DisplayServer.clipboard_get()
	_check(clip.strip_edges() == String(original).strip_edges(), "Copy puts the share string on the clipboard (%d chars)" % clip.length())
	await _key(KEY_ESCAPE)
	await _wait(0.6)
	_check(Screens.top_name() == &"dry_dock", "Esc closes the share dialog")
	await _click(file)
	await _wait(0.3)
	await _menu_focus(popup, popup.get_item_index(5))
	await _key(KEY_ENTER)
	await _wait(1.0)
	await _shot("share_import")
	top = Screens.top()
	_check(Screens.top_name() == &"share_dialog", "File, Import a share string opens the import")
	primary = top.get(&"_primary") as Button
	await _click(primary)
	await _wait(1.0)
	await _shot("share_imported")
	var open := top.get(&"_open") as Button
	_check(open != null and open.visible, "the import offers Open in dry dock")
	if open != null and open.visible:
		await _click(open)
		await _wait(0.5)
		await _answer_dialog(&"open")
		await _wait(2.0)
		await _shot("share_opened")
		print("dock_play: opened ", Editor.design_path())
		_check(String(Editor.design_path()).contains("Imported"), "the imported design opens from Imported")


func _plan_modes() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var graph_button := dock.get_node(^"Scroll/Frame/Body/Centre/ViewBar/Margin/Row/GraphMode") as Button
	await _click(graph_button)
	await _wait(1.5)
	await _shot("graph_mode")
	var scripts_button := dock.get_node(^"Scroll/Frame/Body/Centre/ViewBar/Margin/Row/ScriptMode") as Button
	if scripts_button.visible:
		await _click(scripts_button)
		await _wait(1.0)
		await _shot("scripts_mode")
	await _click(dock.get_node(^"Scroll/Frame/Body/Centre/ViewBar/Margin/Row/ShipMode") as Button)
	await _wait(0.5)


func _plan_camera() -> void:
	var dock := await _open_dock_from_menu()
	if dock == null:
		return
	var stage := dock.get_node(^"Scroll/Frame/Body/Centre/Stage") as Control
	var c := stage.get_global_rect().get_center()
	for i in 10:
		await _pan_gesture(c, Vector2(3, 0))
	await _wait(0.4)
	await _shot("orbit_trackpad")
	for i in 10:
		await _magnify(c, 1.05)
	await _wait(0.4)
	await _shot("pinch_zoom")
	for i in 10:
		await _pan_gesture(c, Vector2(0, 3), true)
	await _wait(0.4)
	await _shot("pan_trackpad")
	for i in 6:
		await _wheel(c, false)
	await _wait(0.4)
	await _shot("wheel_out")


# --- reading state ---------------------------------------------------------------------------------

func _log_state(what: String) -> void:
	var g := Editor.gauges()
	var parts := Editor.parts()
	var info := Editor.info()
	print("dock_play: [", what, "] name=", info.get("name"), " parts=", parts.size(), " dirty=", Editor.is_dirty(),
		" tool=", Editor.tool(), " notes=", Editor.notes().size())
	for k: String in ["float", "speed", "power"]:
		if g.has(k):
			print("    ", k, ": ", str(g[k]).left(300))


func _log_ghost(what: String) -> void:
	var g: Variant = Editor.ghost()
	print("dock_play: ghost ", what, ": ", str(g).left(400))


func _log_notes() -> void:
	for n: Dictionary in Editor.notes():
		print("    note: ", Tx.text(n.get("text", {})) if Tx.has_method(&"text") else str(n).left(300))


func _handle(dock: Control, id: String) -> Dictionary:
	var handles: Array = dock.get(&"_handles")
	var station := int(dock.get_node(^"Scroll/Frame/Body/LeftDock/Tabs/Hull").call(&"current_station"))
	for h: Dictionary in handles:
		if String(h.get("id", "")) == id and int(h.get("station", -1)) == station:
			return h
	for h: Dictionary in handles:
		if String(h.get("id", "")) == id:
			return h
	return {}


func _display_to_canvas(dock: Control, p: Vector2) -> Vector2:
	var display := dock.get_node(^"Scroll/Frame/Body/Centre/Stage/Display") as Control
	return display.get_global_transform_with_canvas() * p


func _tray_pick(tray: Node, words: Array) -> bool:
	var tiles := tray.find_children("*", "Button", true, false)
	for t: Node in tiles:
		var b := t as Button
		if not b.is_visible_in_tree():
			continue
		var item: Variant = b.get(&"item")
		if not item is Dictionary:
			continue
		var text := (String((item as Dictionary).get("id", "")) + " " + _label_text(b)).to_lower()
		var all := true
		for w: String in words:
			if not text.contains(w):
				all = false
		if all:
			print("dock_play: tray pick '", _label_text(b), "' ", (item as Dictionary).get("id", ""))
			await _scroll_into_view(b)
			await _click(b)
			return true
	print("dock_play: tray has nothing for ", words)
	return false


static func _label_text(c: Node) -> String:
	var out := ""
	for l: Node in c.find_children("*", "Label", true, false):
		out += (l as Label).text + " "
	return out.strip_edges()


func _scroll_into_view(c: Control) -> void:
	var p := c.get_parent()
	while p != null and not p is ScrollContainer:
		p = p.get_parent()
	if p is ScrollContainer:
		(p as ScrollContainer).ensure_control_visible(c)
		await _frames(3)


func _tab(dock: Control, path: String, index: int) -> void:
	var tabs := dock.get_node(NodePath(path)) as TabContainer
	var bar := tabs.get_tab_bar()
	var r := bar.get_tab_rect(index)
	await _click_at(bar.get_global_transform_with_canvas() * r.get_center())


## Pick a menu item by clicking where it is drawn.
func _menu_pick(popup: PopupMenu, index: int) -> void:
	if not popup.visible:
		popup.id_pressed.emit(popup.get_item_id(index))
		return
	# Popups are their own windows; drive them by keyboard focus, which is how a player without a
	# pointer picks too.
	popup.set_focused_item(index)
	await _frames(2)
	var ev := InputEventKey.new()
	ev.keycode = KEY_ENTER
	ev.physical_keycode = KEY_ENTER
	ev.pressed = true
	popup.push_input(ev)
	await _frames(2)
	ev = ev.duplicate() as InputEventKey
	ev.pressed = false
	popup.push_input(ev)
	await _frames(2)


## Walk an open popup's highlight down to `index` with the arrow key, as a keyboard player does.
func _menu_focus(popup: PopupMenu, index: int) -> void:
	for i in 24:
		if popup.get_focused_item() == index:
			return
		await _key(KEY_DOWN)
	_fail("the menu never reached item %d" % index)


func _answer_dialog(choice: StringName) -> void:
	await _frames(4)
	var b := get_tree().root.find_child("Choice_" + String(choice), true, false) as Button
	if b == null or not b.is_visible_in_tree():
		print("dock_play: no dialog asking ", choice)
		return
	await _shot("dialog_" + String(choice))
	await _click(b)


# --- input -----------------------------------------------------------------------------------------

func _to_window(p: Vector2) -> Vector2:
	return get_tree().root.get_final_transform() * p


func _move(p: Vector2, button_mask: int = 0) -> void:
	var ev := InputEventMouseMotion.new()
	ev.position = _to_window(p)
	ev.global_position = ev.position
	ev.button_mask = button_mask
	Input.parse_input_event(ev)
	await _frames(1)


func _button(p: Vector2, pressed: bool, button: MouseButton = MOUSE_BUTTON_LEFT, shift: bool = false) -> void:
	var ev := InputEventMouseButton.new()
	ev.position = _to_window(p)
	ev.global_position = ev.position
	ev.button_index = button
	ev.pressed = pressed
	ev.shift_pressed = shift
	ev.button_mask = (1 << (button - 1)) if pressed else 0
	Input.parse_input_event(ev)
	await _frames(1)


func _click_at(p: Vector2, button: MouseButton = MOUSE_BUTTON_LEFT, shift: bool = false) -> void:
	await _move(p)
	await _button(p, true, button, shift)
	await _button(p, false, button, shift)
	await _frames(2)


func _click(c: Control) -> void:
	if c == null:
		_fail("click on a missing control")
		return
	await _click_at(c.get_global_rect().get_center())


func _drag(from: Vector2, to: Vector2, steps: int, button: MouseButton = MOUSE_BUTTON_LEFT) -> void:
	await _move(from)
	await _button(from, true, button)
	for i in range(1, steps + 1):
		await _move(from.lerp(to, float(i) / float(steps)), 1 << (button - 1))
	await _button(to, false, button)
	await _frames(2)


func _wheel(p: Vector2, up: bool) -> void:
	await _button(p, true, MOUSE_BUTTON_WHEEL_UP if up else MOUSE_BUTTON_WHEEL_DOWN)
	await _button(p, false, MOUSE_BUTTON_WHEEL_UP if up else MOUSE_BUTTON_WHEEL_DOWN)


func _pan_gesture(p: Vector2, delta: Vector2, shift: bool = false) -> void:
	var ev := InputEventPanGesture.new()
	ev.position = _to_window(p)
	ev.delta = delta
	ev.shift_pressed = shift
	Input.parse_input_event(ev)
	await _frames(1)


func _magnify(p: Vector2, factor: float) -> void:
	var ev := InputEventMagnifyGesture.new()
	ev.position = _to_window(p)
	ev.factor = factor
	Input.parse_input_event(ev)
	await _frames(1)


func _key(code: Key, shift: bool = false, command: bool = false) -> void:
	var ev := InputEventKey.new()
	ev.keycode = code
	ev.physical_keycode = code
	ev.shift_pressed = shift
	if OS.get_name() == "macOS":
		ev.meta_pressed = command
	else:
		ev.ctrl_pressed = command
	ev.command_or_control_autoremap = false
	ev.pressed = true
	Input.parse_input_event(ev)
	await _frames(1)
	var up := ev.duplicate() as InputEventKey
	up.pressed = false
	Input.parse_input_event(up)
	await _frames(1)


func _type(text: String) -> void:
	for ch in text:
		var ev := InputEventKey.new()
		ev.unicode = ch.unicode_at(0)
		ev.keycode = OS.find_keycode_from_string(ch.to_upper())
		ev.pressed = true
		Input.parse_input_event(ev)
		await _frames(1)
		var up := ev.duplicate() as InputEventKey
		up.pressed = false
		Input.parse_input_event(up)
		await _frames(1)


# --- timing and capture ----------------------------------------------------------------------------

func _frames(n: int) -> void:
	for i in n:
		await get_tree().process_frame


func _wait(seconds: float) -> void:
	await get_tree().create_timer(seconds, true, false, true).timeout


func _shot(name: String) -> void:
	await RenderingServer.frame_post_draw
	_step += 1
	var path := _out.path_join("%02d_%s.png" % [_step, name])
	get_tree().root.get_texture().get_image().save_png(path)
	print("dock_play: shot ", path)


func _fail(why: String) -> void:
	_failed = true
	push_error("dock_play: " + why)


static func _slug(s: String) -> String:
	return s.to_lower().validate_filename().replace(" ", "_").left(24)
