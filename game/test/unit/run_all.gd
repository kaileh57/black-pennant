extends SceneTree
## Runs every headless suite in res://test/unit (each `*_test.gd`, one Godot process apiece so
## one suite's autoload state never leaks into the next) and gates on their exit codes. Unit:
## ui-tests. Stands in for GdUnit4's command-line runner until the add-on is vendored (DESIGN.md
## L1304), with the same contract: exit 0 only when every suite passed, JUnit XML on request.
##   godot --headless --path . -s res://test/unit/run_all.gd [-- --junit=/abs/report.xml] [--only=a,b]
## Every suite runs with HOME, XDG_DATA_HOME and APPDATA pointed at a fresh folder under the OS
## temp folder, so suites that write saves or settings never touch the player's user folder (the
## Windows path through APPDATA is untested here).
## `lobby_test` hosts and plays a solo skirmish on 127.0.0.1 on a free port (it needs arguments).

const DIR := "res://test/unit"
## Extra arguments a suite needs; `{port}` becomes a free UDP port.
const ARGS: Dictionary[String, Array] = {
	"lobby_test": ["--", "--role=solo", "--mode=skirmish", "--port={port}", "--loopback-host"],
}


func _initialize() -> void:
	var junit := ""
	var only := PackedStringArray()
	for arg: String in OS.get_cmdline_user_args():
		if arg.begins_with("--junit="):
			junit = arg.trim_prefix("--junit=")
		elif arg.begins_with("--only="):
			only = arg.trim_prefix("--only=").split(",", false)
	var suites := PackedStringArray()
	for f: String in DirAccess.get_files_at(DIR):
		if f.ends_with("_test.gd") and (only.is_empty() or f.trim_suffix(".gd") in only):
			suites.append(f.trim_suffix(".gd"))
	suites.sort()
	var home := OS.get_temp_dir().path_join("bp-test-home-%d" % OS.get_process_id())
	DirAccess.make_dir_recursive_absolute(home)
	# Godot finds user:// under HOME on macOS, XDG_DATA_HOME (else HOME) on Linux and APPDATA on
	# Windows; point all of them at the scratch folder.
	OS.set_environment("HOME", home)
	OS.set_environment("USERPROFILE", home)
	OS.set_environment("XDG_DATA_HOME", home.path_join("data"))
	OS.set_environment("APPDATA", home.path_join("AppData"))
	var project := ProjectSettings.globalize_path("res://")
	var results: Array[Dictionary] = []
	var failed := 0
	for suite: String in suites:
		var args := PackedStringArray(["--headless", "--path", project, "-s", "%s/%s.gd" % [DIR, suite]])
		for a: Variant in ARGS.get(suite, []):
			args.append(String(a).replace("{port}", str(40_000 + (OS.get_process_id() + suites.find(suite) * 97) % 20_000)))
		var out: Array = []
		var start := Time.get_ticks_msec()
		var code := OS.execute(OS.get_executable_path(), args, out, true)
		var seconds := (Time.get_ticks_msec() - start) / 1000.0
		var text := "" if out.is_empty() else String(out[0])
		# A suite whose script fails to parse never runs, and Godot still exits 0 for it: count it failed.
		var ok := code == 0 and not text.contains("Parse Error") and not text.contains("Failed to load script")
		failed += 0 if ok else 1
		results.append({"name": suite, "ok": ok, "code": code, "seconds": seconds, "text": text})
		print("%s %s (%.1f s)" % ["pass" if ok else "FAIL", suite, seconds])
		if not ok:
			for line: String in text.split("\n"):
				if line.contains("FAIL") or line.contains("SCRIPT ERROR") or line.contains("Parse Error"):
					print("    ", line.strip_edges())
	print("run_all: %d suites, %d failed" % [suites.size(), failed])
	if not junit.is_empty():
		_write_junit(junit, results, failed)
	quit(1 if failed > 0 or suites.is_empty() else 0)


func _write_junit(path: String, results: Array[Dictionary], failed: int) -> void:
	var total := 0.0
	for r: Dictionary in results:
		total += float(r["seconds"])
	var xml := PackedStringArray(['<?xml version="1.0" encoding="UTF-8"?>'])
	xml.append('<testsuites name="godot-unit" tests="%d" failures="%d" time="%.3f">' % [results.size(), failed, total])
	xml.append('<testsuite name="res://test/unit" tests="%d" failures="%d" time="%.3f">' % [results.size(), failed, total])
	for r: Dictionary in results:
		xml.append('<testcase classname="test.unit" name="%s" time="%.3f">' % [String(r["name"]).xml_escape(true), float(r["seconds"])])
		if not bool(r["ok"]):
			xml.append('<failure message="exit code %d">%s</failure>' % [int(r["code"]), String(r["text"]).right(20_000).xml_escape()])
		xml.append("</testcase>")
	xml.append("</testsuite>")
	xml.append("</testsuites>")
	var f := FileAccess.open(path, FileAccess.WRITE)
	if f == null:
		printerr("run_all: cannot write ", path)
		return
	f.store_string("\n".join(xml) + "\n")
