#!/usr/bin/env bash
# tier: T1
# covers: spira/testlib.sh release/src/stage.rs
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT INT TERM

PROD="$T/prod"
mkdir "$T/f"
git init -q -b main "$PROD"
bare_before="$(git --git-dir="$PROD/.git" config core.bare)"
is "fixture starts non-bare" "false" "$bare_before"

# Positive control: the polluted environment really does flip the flag when nothing fences it.
CTRL="$T/ctrl"; git init -q -b main "$CTRL"
( cd "$T/f" && env -i PATH="$PATH" HOME="$T" GIT_DIR="$CTRL/.git" git init -q --bare )
is "control: unfenced GIT_DIR flips core.bare" "true" "$(git --git-dir="$CTRL/.git" config core.bare)"

# The fence: a suite that sources testlib.sh under a polluted environment cannot reach it.
out="$(cd "$T/f" && env -i PATH="$PATH" HOME="$T" GIT_DIR="$PROD/.git" bash -c '
  . "'"$HERE"'/testlib.sh" >/dev/null
  git init -q --bare -b main && git config user.name t
  echo "GIT_DIR=${GIT_DIR:-unset}"' 2>&1)"
want "testlib unsets GIT_DIR" "GIT_DIR=unset" "$out"
is "production core.bare unchanged" "false" "$(git --git-dir="$PROD/.git" config core.bare)"
is "fixture identity not written" "" "$(git --git-dir="$PROD/.git" config --local user.name || true)"

tl_summary
