#!/usr/bin/env bash
#
# test-conf-watch.sh — loom.sh notices, once, when its config's write target changes under it
# (law-long-lived-processes-pin-their-config), logs "config changed" and exits 0 for systemd
# (Restart=always) to restart it. The tick is injected through SPIRA_LOOM_TICK. Since the one
# source of config (per Ryan 2026-10-05), loom.sh watches spira_toml_write_target() — the LAST
# layer of SPIRA_TOML, i.e. this suite's own override file — rather than a standalone SPIRA_CONF
# env var, so the change case touches that file directly.
#
# The change is never timed: the fake Loom binary touches an "armed" marker, which loom.sh
# spawns strictly after capturing its conf-mtime baseline, and the config is touched only once
# that marker exists. A fixed-delay touch raced the baseline capture under a loaded corpus and
# left the loop silent for its whole window.
#
# tier: T1
# covers: spira/loom.sh spira/conf.sh UC-cockpit-observability-07
# host-reason: runs loom.sh as a real child process against a temp filesystem; no writes outside $TMP
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
BASE_PATH="$PATH"

# A fake Loom binary that arms, then loops until killed; loom.sh starts it after its baseline.
FAKE_DIR="$TMP/fake-bin"; mkdir -p "$FAKE_DIR"
FAKE_BIN="$FAKE_DIR/loom"
printf '#!/bin/sh\ntouch "$ARMED_MARKER" 2>/dev/null\nsleep 30\n' > "$FAKE_BIN"
chmod +x "$FAKE_BIN"

# The positive control always waits its whole window (timeout must expire to prove nothing
# exited early); the change case returns as soon as the loop notices.
TICK=0.2
WIN_STAY=0.6
# Startup (sourcing conf.sh) can exceed 3s under a loaded corpus; the window costs time only on failure.
WIN_CHANGE=15

# wait_for_marker <marker> -> 0 once <marker> exists, 1 after ~10s. The bounded poll idiom
# already used elsewhere in this tree (test-thrash-teardown.sh) for "wait on a real signal",
# never a fixed sleep guessing how long the signal takes to arrive.
wait_for_marker() {
    local marker="$1" waited=0
    while [ ! -e "$marker" ] && [ "$waited" -lt 100 ]; do
        sleep 0.1
        waited=$((waited + 1))
    done
    [ -e "$marker" ]
}

run_loom() {      # run_loom <window> <marker>
    # SPIRA_RUN/SPIRA_DB/SPIRA_REPO_MAP/SPIRA_FAYTHS are registered keys (per Ryan
    # 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config and thread SPIRA_TOML
    # through env -i, which clears it. There is no separate SPIRA_CONF env var any more;
    # loom.sh's watched path is $_TL_CONF_OVERRIDE (SPIRA_TOML's last layer).
    tl_config SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" SPIRA_FAYTHS=t
    # Back-date the override file tl_config just wrote (same trick the old CONF_CHG fixture
    # used): everything here runs well under 1s, so without this the baseline loom.sh
    # captures and the later real touch can land in the same integer mtime second and the
    # change goes unnoticed.
    touch -d "3 seconds ago" "$_TL_CONF_OVERRIDE"
    timeout "$1" env -i PATH="$FAKE_DIR:$BASE_PATH" HOME="$RUN" LC_ALL=C.UTF-8 \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_GOAL=sp-test \
        ARMED_MARKER="${2:-}" \
        SPIRA_LOOM_TICK="$TICK" \
        SPIRA_TOML="$SPIRA_TOML" \
        bash "$HERE/loom.sh"
}

for name in loom; do
    label="loom.sh"

    RUN="$TMP/$name-run"; mkdir -p "$RUN/cockpit.d"

    # ── NEGATIVE CASE: no config change — the loop stays running ──────────────────────
    ec_stay=0
    "run_$name" "$WIN_STAY" >/dev/null 2>/dev/null || ec_stay=$?
    is "$label: no config change — stays running" "124" "$ec_stay"

    # ── CHANGE CASE: touch the override file only once the loop has armed ─────────────
    marker="$RUN/armed"
    err_log="$TMP/$name-err.log"
    "run_$name" "$WIN_CHANGE" "$marker" >/dev/null 2>"$err_log" &
    run_pid=$!
    if wait_for_marker "$marker"; then
        ok "$label: config change — armed before the touch"
    else
        bad "$label: config change — armed before the touch" "marker never appeared within 10s"
    fi
    touch "$_TL_CONF_OVERRIDE"
    ec_chg=0
    wait "$run_pid" || ec_chg=$?
    is "$label: config change — exits 0" "0" "$ec_chg"
    want "$label: config change — message on stderr" "config changed" "$(cat "$err_log" 2>/dev/null)"
done

tl_summary
