#!/usr/bin/env bash
#
# test-loom-conf-change.sh — loom.sh exits cleanly when the config file changes.
#
# THE FAILURE THIS SUITE EXISTS FOR. loom.sh used to exec the binary directly. Once
# exec'd, the binary held its environment variables (SPIRA_RUN, SPIRA_CTRL, etc.) from
# the moment it started. A re-pin of any path key left the running server pointing into
# a stale location until restarted by hand. collect.sh already solves this by checking
# the config mtime on every tick; loom.sh now does the same (law-long-lived-processes-pin-their-config).
#
# WHAT IS EXERCISED:
#   1. NEGATIVE CASE (positive control): loom.sh stays running when config is unchanged.
#   2. CHANGE CASE: loom.sh exits 0 and emits the expected message when config mtime changes.
#
# covers: spira/loom.sh
# host-reason: runs loom.sh as a child process against a fake binary; no filesystem writes outside $TMP
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A fake binary that loops until killed (simulates the Loom server).
FAKE_BIN="$TMP/fake-loom"
printf '#!/bin/sh\nsleep 30\n' > "$FAKE_BIN"
chmod +x "$FAKE_BIN"

BASE_PATH="$PATH"

# ── NEGATIVE CASE: no config change — loom.sh must not exit before timeout ────────────
echo "1. no config change — loom.sh stays running"

CONF_NOCHANGE="$TMP/nochange.conf"
printf '# test\n' > "$CONF_NOCHANGE"
_ec_nochange=0
timeout 3 env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$CONF_NOCHANGE" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_LOOM_BIN="$FAKE_BIN" \
    SPIRA_LOOM_TICK=1 \
    bash "$HERE/loom.sh" >/dev/null 2>/dev/null || _ec_nochange=$?
# timeout exits 124 when the child was still running when the clock expired.
if [ "$_ec_nochange" -eq 124 ]; then
    ok "no config change: loom.sh stayed running until timeout"
else
    bad "no config change: loom.sh exited early" "exit $_ec_nochange"
fi

# ── CHANGE CASE: touch the config file → loom.sh exits 0 ─────────────────────────────
echo "2. config change — loom.sh exits 0 and logs the change"

CONF_CHANGE="$TMP/change.conf"
printf '# test\n' > "$CONF_CHANGE"
# Pre-date so the touch below produces a genuine mtime change.
touch -d "10 seconds ago" "$CONF_CHANGE"
( sleep 2; touch "$CONF_CHANGE" ) &
_touch_pid=$!
_ec_change=0
timeout 8 env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$CONF_CHANGE" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_LOOM_BIN="$FAKE_BIN" \
    SPIRA_LOOM_TICK=1 \
    bash "$HERE/loom.sh" >/dev/null 2>"$TMP/loom_err.log" || _ec_change=$?
wait "$_touch_pid" 2>/dev/null || true

if [ "$_ec_change" -eq 0 ]; then
    ok "config change: loom.sh exits 0"
else
    bad "config change: loom.sh exited $_ec_change" "wanted 0"
fi
want "config change: message emitted on stderr" "config changed" \
    "$(cat "$TMP/loom_err.log" 2>/dev/null)"

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
