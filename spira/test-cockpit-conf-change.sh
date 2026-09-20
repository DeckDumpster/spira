#!/usr/bin/env bash
#
# test-cockpit-conf-change.sh — health.sh exits cleanly when the config file changes.
#
# THE FAILURE THIS SUITE EXISTS FOR. health.sh runs for days as the ops pane renderer.
# It sources conf.sh once at startup and then holds all resolved paths (SPIRA_RUN in
# particular) for the life of the process. A re-pin of SPIRA_RUN left the renderer
# reading cockpit.env from the stale location, rendering ? in every field while the
# collector wrote a good snapshot at the new one (law-long-lived-processes-pin-their-config).
#
# WHAT IS EXERCISED:
#   1. NEGATIVE CASE (positive control): health.sh stays running when config is unchanged.
#   2. CHANGE CASE: health.sh exits 0 and emits the expected message when config mtime changes.
#
# covers: cockpit/health.sh
# host-reason: runs health.sh in loop mode against a temp filesystem; no live snapshot written
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HEALTH="$HERE/../cockpit/health.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/run" "$TMP/home"

# A mock systemctl that always reports active, so halt_banner does not render.
printf '#!/bin/sh\necho active\n' > "$TMP/mock-systemctl"
chmod +x "$TMP/mock-systemctl"

BASE_PATH="$PATH"

# ── NEGATIVE CASE: no config change — health.sh stays running ─────────────────────────
echo "1. no config change — health.sh stays running"

CONF_NOCHANGE="$TMP/nochange.conf"
printf '# test\n' > "$CONF_NOCHANGE"
_ec_nochange=0
timeout 4 env -i PATH="$BASE_PATH" HOME="$TMP/home" TERM=dumb LC_ALL=C.UTF-8 \
    SPIRA_CONF="$CONF_NOCHANGE" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_SYSTEMCTL="$TMP/mock-systemctl" \
    bash "$HEALTH" loop >/dev/null 2>/dev/null || _ec_nochange=$?
if [ "$_ec_nochange" -eq 124 ]; then
    ok "no config change: health.sh stayed running until timeout"
else
    bad "no config change: health.sh exited early" "exit $_ec_nochange"
fi

# ── CHANGE CASE: touch the config file → health.sh exits 0 ───────────────────────────
echo "2. config change — health.sh exits 0 and logs the change"

CONF_CHANGE="$TMP/change.conf"
printf '# test\n' > "$CONF_CHANGE"
# Pre-date so the touch below produces a genuine mtime change.
touch -d "10 seconds ago" "$CONF_CHANGE"
( sleep 1; touch "$CONF_CHANGE" ) &
_touch_pid=$!
_ec_change=0
timeout 6 env -i PATH="$BASE_PATH" HOME="$TMP/home" TERM=dumb LC_ALL=C.UTF-8 \
    SPIRA_CONF="$CONF_CHANGE" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$TMP/run" SPIRA_DB="$TMP/nodb" \
    SPIRA_REPO_MAP="$TMP/no-map" SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
    SPIRA_SYSTEMCTL="$TMP/mock-systemctl" \
    bash "$HEALTH" loop >/dev/null 2>"$TMP/health_err.log" || _ec_change=$?
wait "$_touch_pid" 2>/dev/null || true

if [ "$_ec_change" -eq 0 ]; then
    ok "config change: health.sh exits 0"
else
    bad "config change: health.sh exited $_ec_change" "wanted 0"
fi
want "config change: message emitted on stderr" "config changed" \
    "$(cat "$TMP/health_err.log" 2>/dev/null)"

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
