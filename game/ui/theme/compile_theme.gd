@tool
extends SceneTree
## Compile res://ui/theme/tokens.json into the project themes (DESIGN.md L1727). From game/:
##   godot --headless --path . -s res://ui/theme/compile_theme.gd            write the themes
##   godot --headless --path . -s res://ui/theme/compile_theme.gd -- --check fail if stale
## Exit code 0 on success, 1 on any error or stale output. Unit: ui-theme.

const Compiler := preload("res://ui/theme/theme_compiler.gd")


func _initialize() -> void:
	var check := OS.get_cmdline_user_args().has("--check")
	var compiler := Compiler.new()
	var ok := true
	if check:
		var stale := compiler.stale_outputs()
		for path: String in stale:
			printerr("compile_theme: stale ", path, "; run res://ui/theme/compile_theme.gd")
		ok = stale.is_empty()
	else:
		ok = compiler.write_all()
		if ok:
			print("compile_theme: wrote ", ", ".join(compiler.OUTPUTS.values()))
	for message: String in compiler.errors:
		printerr("compile_theme: ", message)
	quit(0 if ok and compiler.errors.is_empty() else 1)
