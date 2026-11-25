#!/bin/sh
# Fast between-workflow gate. Usage: tools/check.sh [--no-test] [--no-godot]
# Prints one line per check and exits non-zero if any hard check fails.
cd "$(git rev-parse --show-toplevel)" || exit 2
fail=0
note() { printf '%-28s %s\n' "$1" "$2"; }

if cargo build --workspace >/tmp/bp-build.log 2>&1; then note build ok; else note build FAIL; tail -30 /tmp/bp-build.log; fail=1; fi

case " $* " in *" --no-test "*) note test skipped;; *)
  if cargo test --workspace >/tmp/bp-test.log 2>&1; then
    note test "ok ($(grep -c '^test .* ok$' /tmp/bp-test.log) passed)"
  else note test FAIL; grep -E '^test .* FAILED|panicked|^error' /tmp/bp-test.log | head -40; fail=1; fi;;
esac

# Determinism: no std transcendentals in the simulation crates.
t=$(grep -rnE '\.(sin|cos|tan|asin|acos|atan|atan2|sinh|cosh|tanh|exp|exp2|exp_m1|ln|ln_1p|log|log2|log10|powf|powi|cbrt|hypot|sin_cos)\(' crates/sim-core/src crates/sim-systems/src 2>/dev/null | grep -v 'libm::' | grep -vE '^\S+:\s*//' | wc -l | tr -d ' ')
[ "$t" = 0 ] && note std-transcendentals ok || { note std-transcendentals "FAIL ($t)"; fail=1; }
h=$(grep -rnE 'HashMap|HashSet' crates/sim-core/src crates/sim-systems/src 2>/dev/null | grep -vE '^\S+:\s*//' | wc -l | tr -d ' ')
[ "$h" = 0 ] && note hashmap-in-sim ok || { note hashmap-in-sim "FAIL ($h)"; fail=1; }
g=$(grep -rn 'godot' crates/*/Cargo.toml | grep -v gdclient | wc -l | tr -d ' ')
[ "$g" = 0 ] && note godot-boundary ok || { note godot-boundary FAIL; fail=1; }

# UI rules.
c() { grep -rnE "$1" game --include='*.gd' --include='*.tscn' --include='*.tres' --include='*.gdshader' 2>/dev/null | grep -v '^game/_placeholder/' | grep -v '^game/addons/' | wc -l | tr -d ' '; }
n=$(c 'theme_override_'); [ "$n" = 0 ] && note theme-override ok || { note theme-override "FAIL ($n)"; fail=1; }
n=$(grep -rnE 'Color\(|Color8\(|#[0-9A-Fa-f]{6}\b' game --include='*.gd' --include='*.tscn' 2>/dev/null | grep -v 'game/ui/theme/' | grep -v '^game/world/' | grep -v "^game/test/" | wc -l | tr -d ' ')
[ "$n" = 0 ] && note ui-colour-literals ok || note ui-colour-literals "WARN ($n)"
n=$(c 'TRANS_(ELASTIC|BOUNCE|BACK|SPRING)'); [ "$n" = 0 ] && note tween-transitions ok || { note tween-transitions "FAIL ($n)"; fail=1; }
n=$(c 'uppercase = true'); [ "$n" = 0 ] && note label-uppercase ok || { note label-uppercase "FAIL ($n)"; fail=1; }
n=$(c 'RigidBody3D|CharacterBody3D'); [ "$n" = 0 ] && note godot-physics ok || { note godot-physics "FAIL ($n)"; fail=1; }
n=$(grep -rniwE 'introducing|unlock|unlocks|unlocked|empower|elevate|streamline|transform|discover|seamless|smart|next-gen|revolutionize|leverage|delve|vibrant|pivotal|showcase|tapestry|testament|unleash' game/i18n README.md 2>/dev/null | wc -l | tr -d ' ')
[ "$n" = 0 ] && note banned-words ok || note banned-words "WARN ($n)"

s=$(grep -rnE 'todo!\(|unimplemented!\(' crates --include='*.rs' 2>/dev/null | wc -l | tr -d ' ')
note stubs-remaining "$s"

# Windows cross-check from this Mac. Only crates without C build scripts check here: every other crate reaches
# zstd-sys through content, and its C build needs the MSVC CRT headers, which macOS lacks. Windows CI builds
# the whole workspace natively.
if rustup target list --installed 2>/dev/null | grep -q x86_64-pc-windows-msvc; then
  if cargo check -p audio --all-targets --target x86_64-pc-windows-msvc --target-dir target/wincheck >/tmp/bp-wincheck.log 2>&1; then note windows-check "ok (audio)"; else note windows-check FAIL; tail -20 /tmp/bp-wincheck.log; fail=1; fi
else note windows-check "skipped (no x86_64-pc-windows-msvc target)"; fi

case " $* " in *" --no-godot "*) note godot-smoke skipped;; *)
  (cd game && godot --headless --path . -- --smoke >/tmp/bp-godot.log 2>&1)
  e=$(grep -cE 'ERROR|SCRIPT ERROR|Parse Error|panicked' /tmp/bp-godot.log)
  [ "$e" = 0 ] && note godot-smoke ok || { note godot-smoke "FAIL ($e)"; grep -E 'ERROR|Parse Error|panicked' /tmp/bp-godot.log | head -20; fail=1; }
  ;;
esac
exit $fail
