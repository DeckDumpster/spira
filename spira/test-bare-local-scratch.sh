#!/usr/bin/env bash
# test-bare-local-scratch.sh — bare-local scratch dirs are removed on every exit path, and the
# reaper removes a stale read-only one while leaving a held one.
# tier: T1
# covers: spira/bare-local.sh spira/bare-local-scratch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
echo "test-bare-local-scratch.sh"

ROOT="$(mktemp -d)"
trap 'chmod -R u+w "$ROOT"; rm -rf "$ROOT"' EXIT

count() { find "$ROOT" -maxdepth 1 -name 'bare-local.*' | wc -l; }
ro_tree() { mkdir -p "$1/releases/abc" && echo x > "$1/releases/abc/f" && chmod -R a-w "$1/releases"; }

body='. "$1/bare-local-scratch.sh"; bare_local_scratch_new "$2" || exit 9; ro_tree() { mkdir -p "$scratch/releases/abc" && echo x > "$scratch/releases/abc/f" && chmod -R a-w "$scratch/releases"; }; ro_tree'

bash -c "$body"' ; exit 0' _ "$HERE" "$ROOT"; rc=$?
is "clean exit leaves no dir" "0 0" "$rc $(count)"

bash -c "$body"' ; exit 3' _ "$HERE" "$ROOT"; rc=$?
is "nonzero exit leaves no dir" "3 0" "$rc $(count)"

bash -c "$body"' ; sleep 30 & wait' _ "$HERE" "$ROOT" &
pid=$!
for _ in $(seq 50); do [ "$(count)" = 1 ] && [ -e "$(echo "$ROOT"/bare-local.*)/releases/abc/f" ] && break; sleep 0.1; done
is "positive control: dir exists while running" "1" "$(count)"
kill -TERM "$pid"; wait "$pid" 2>/dev/null
is "TERM leaves no dir" "0" "$(count)"
pkill -f 'sleep 30' 2>/dev/null

. "$HERE/bare-local-scratch.sh"
mkdir "$ROOT/bare-local.stale1"; ro_tree "$ROOT/bare-local.stale1"
mkdir "$ROOT/bare-local.fresh1"; ro_tree "$ROOT/bare-local.fresh1"
mkdir "$ROOT/bare-local.held11"; : > "$ROOT/bare-local.held11/.lock"; ro_tree "$ROOT/bare-local.held11"
touch -d '3 hours ago' "$ROOT/bare-local.stale1" "$ROOT/bare-local.held11"
exec 7>"$ROOT/bare-local.held11/.lock"; flock -n 7
bare_local_reap "$ROOT" 7200 2>/dev/null
is "stale read-only dir reaped" "no" "$([ -e "$ROOT/bare-local.stale1" ] && echo yes || echo no)"
is "held dir kept" "yes" "$([ -e "$ROOT/bare-local.held11" ] && echo yes || echo no)"
is "fresh dir kept" "yes" "$([ -e "$ROOT/bare-local.fresh1" ] && echo yes || echo no)"
exec 7>&-
bare_local_reap "$ROOT" 7200 2>/dev/null
is "released stale dir reaped" "no" "$([ -e "$ROOT/bare-local.held11" ] && echo yes || echo no)"

tl_summary
