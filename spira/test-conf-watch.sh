#!/usr/bin/env bash
#
# test-conf-watch.sh — loom.sh hands the process to loom and starts nothing of its own: the
# config watch is loom's (loom/src/confwatch.rs), so an idle Loom forks nothing to hash a file.
#
# tier: T1
# covers: spira/loom.sh loom/src/main.rs loom/src/confwatch.rs spira/conf.sh UC-cockpit-observability-07
# host-reason: runs loom.sh as a real child process against a temp filesystem; no writes outside $TMP
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"
printf '#!/bin/sh\nprintf "pid=%%s args=%%s\\n" "$$" "$*" > "$OUT"\n' > "$TMP/bin/loom"
chmod +x "$TMP/bin/loom"

: > "$TMP/conf.toml"
OUT="$TMP/out" PATH="$TMP/bin:$PATH" SPIRA_TOML="$TMP/conf.toml" bash "$HERE/loom.sh" --flag x >/dev/null 2>&1
rc=$?
is "loom.sh exits with loom's status" "0" "$rc"
out="$(cat "$TMP/out" 2>/dev/null)"
case "$out" in *"args=--flag x") ok "loom.sh passes its arguments through" ;; *) bad "loom.sh passes its arguments through" "$out" ;; esac

if grep -Eq 'sleep|sha256sum|conf_stamp|conf_changed|kill -0' "$HERE/loom.sh"; then
    bad "loom.sh has no poll loop" "$(grep -En 'sleep|sha256sum|conf_stamp|conf_changed|kill -0' "$HERE/loom.sh")"
else
    ok "loom.sh has no poll loop"
fi
if grep -Eq '^exec loom "\$@"$' "$HERE/loom.sh"; then ok "loom.sh execs loom"; else bad "loom.sh execs loom" "no exec line"; fi

tl_summary
