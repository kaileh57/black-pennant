#!/bin/sh
# Rollback snapshot that moves no branch: writes the whole working tree to refs/snapshots/<name>.
set -e
cd "$(git rev-parse --show-toplevel)"
name="${1:?usage: snapshot.sh <name>}"
export GIT_INDEX_FILE="$(mktemp -t bpidx)"
rm -f "$GIT_INDEX_FILE"
git read-tree HEAD
git add -A
tree=$(git write-tree)
commit=$(git commit-tree "$tree" -p HEAD -m "snapshot: $name")
git update-ref "refs/snapshots/$name" "$commit"
rm -f "$GIT_INDEX_FILE"
echo "refs/snapshots/$name -> $commit"
