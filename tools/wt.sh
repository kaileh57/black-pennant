#!/bin/sh
# Merge queue for parallel agents. Work lands on refs/snapshots/int; no branch moves.
#   wt.sh init            snapshot the main checkout as the integration tip
#   wt.sh start <name>    create (or reuse) a worktree at the integration tip; prints its path
#   wt.sh land <name>     commit, rebase onto the tip, cargo check, then advance the tip
#   wt.sh drop <name>     remove a landed worktree and its build output
#   wt.sh sync            copy the integration tree into the main checkout
# The main checkout is the first worktree git lists; agent worktrees live beside it in bp-wt.
MAIN="${BP_MAIN:-$(git -C "$(dirname "$0")" worktree list --porcelain | awk '/^worktree /{print $2; exit}')}"
ROOT="${BP_WT_ROOT:-$(dirname "$MAIN")/bp-wt}"
INT="$ROOT/_int"
REF=refs/snapshots/int
LOCK="$ROOT/.lock"
G="git -C $MAIN"

lock() {
  while ! mkdir "$LOCK" 2>/dev/null; do
    pid=$(cat "$LOCK/pid" 2>/dev/null)
    if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then rm -rf "$LOCK"; continue; fi
    sleep 3
  done
  echo $$ > "$LOCK/pid"
  trap 'rm -rf "$LOCK"' EXIT INT TERM
}
unlock() { rm -rf "$LOCK"; trap - EXIT INT TERM; }

cmd="$1"; name="$2"
mkdir -p "$ROOT"
case "$cmd" in
init)
  lock
  commit=$("$MAIN/tools/snapshot.sh" "int-$(date +%m%d-%H%M%S)" | awk '{print $3}')
  old=$($G rev-parse -q --verify $REF 2>/dev/null)
  if [ -n "$old" ]; then
    # Main-checkout edits made since the last sync go on top of the tip as one commit.
    echo "integration tip already exists; use sync to pull it, or delete $REF to restart" >&2; exit 2
  fi
  $G update-ref $REF "$commit"
  [ -d "$INT" ] || $G worktree add -q --detach "$INT" "$commit"
  git -C "$INT" checkout -q --detach "$commit"
  echo "$REF -> $commit"
  ;;
start)
  [ -n "$name" ] || { echo "usage: wt.sh start <name>" >&2; exit 2; }
  wt="$ROOT/$name"
  if [ ! -d "$wt" ]; then
    lock
    $G worktree add -q --detach "$wt" "$($G rev-parse $REF)" || exit 1
    unlock
    [ -d "$MAIN/target" ] && cp -c -R "$MAIN/target" "$wt/target" 2>/dev/null
  fi
  echo "$wt"
  ;;
land)
  wt="$ROOT/$name"
  [ -d "$wt" ] || { echo "no worktree $wt" >&2; exit 2; }
  cd "$wt" || exit 2
  if [ -d "$(git rev-parse --git-dir)/rebase-merge" ] || [ -d "$(git rev-parse --git-dir)/rebase-apply" ]; then
    echo "a rebase is in progress: resolve the conflicts, run 'git add -A && git -c core.editor=true rebase --continue', then land again" >&2; exit 3
  fi
  git add -A
  git diff --cached --quiet || git commit -q -m "$name" || exit 1
  lock
  tip=$($G rev-parse $REF)
  if [ "$(git merge-base HEAD "$tip")" != "$tip" ]; then
    if ! git -c core.editor=true rebase -q "$tip" >/dev/null 2>&1; then
      unlock
      echo "CONFLICT rebasing onto the integration tip. Conflicted files:" >&2
      git diff --name-only --diff-filter=U >&2
      echo "Resolve them (keep both sides' intent), run 'git add -A && git -c core.editor=true rebase --continue', then land again." >&2
      exit 3
    fi
  fi
  if git diff --name-only "$tip" HEAD | grep -qvE '^(game/|tools/|\.github/|README|DESIGN|CLAUDE|AGENTS)'; then
    if ! cargo check --workspace --all-targets >"$wt/.land-build.log" 2>&1; then
      unlock
      echo "CHECK FAILS after rebasing onto the integration tip (others' landed work is now in your tree): cargo check --workspace --all-targets. Fix it and land again. Errors:" >&2
      grep -E '^error' -A 14 "$wt/.land-build.log" | head -80 >&2
      exit 4
    fi
  fi
  rm -f "$wt/.land-build.log"
  new=$(git rev-parse HEAD)
  $G update-ref $REF "$new"
  git -C "$INT" checkout -q --detach "$new" 2>/dev/null
  unlock
  echo "landed $name at $new"
  ;;
drop)
  wt="$ROOT/$name"
  [ -d "$wt" ] || exit 0
  rm -rf "$wt/target" "$wt/game/.godot"
  $G worktree remove --force "$wt" 2>/dev/null
  echo "dropped $name"
  ;;
sync)
  lock
  git -C "$INT" checkout -q --detach "$($G rev-parse $REF)"
  rsync -a --delete --exclude .git --exclude /target --exclude /game/.godot --exclude /game/shaders/gen --exclude /game/art/gen --exclude /game/audio/gen --exclude /captures --exclude /bench-results "$INT/" "$MAIN/"
  unlock
  echo "main checkout now matches $($G rev-parse --short $REF)"
  ;;
*) echo "usage: wt.sh init|start|land|drop|sync" >&2; exit 2;;
esac
