#!/usr/bin/env bash
#
# test-conf-watch.sh — health.sh, loom.sh and collect.sh all exit cleanly, once, when
# SPIRA_CONF's file changes under them (law-long-lived-processes-pin-their-config).
#
# ONE TABLE OVER THE THREE LOOPS. test-cockpit-conf-change.sh (health.sh), test-loom-
# conf-change.sh (loom.sh) and test-cockpit-tiered-collector.sh::10 (collect.sh) each timed
# the same contract with real `timeout` windows of 3-8s (duplicate cluster 9, docs/test-plan/
# cockpit-observability.md). Each loop's tick is now injected — SPIRA_HEALTH_TICK,
# SPIRA_LOOM_TICK, SPIRA_COCKPIT_TICK — through the shared `conf_changed` helper in conf.sh,
# so both the positive control and the change case run in well under a second per script.
#
# WHAT IS EXERCISED, per script:
#   1. NEGATIVE CASE (positive control): the loop stays running when the config is unchanged.
#   2. CHANGE CASE: the loop exits 0 and logs "config changed" when the config's mtime moves.
#
# tier: T1
# covers: cockpit/health.sh spira/loom.sh spira/collect.sh UC-cockpit-observability-07
# host-reason: runs each script as a real child process against a temp filesystem; no writes outside $TMP
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT_DIR="$(cd "$HERE/../cockpit" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
BASE_PATH="$PATH"

# A fake binary that loops until killed: stands in for the Loom server binary (loom.sh)
# and for the probe subcommand (collect.sh) alike — both loops only need a long-lived child.
FAKE_BIN="$TMP/fake-bin"
printf '#!/bin/sh\nsleep 30\n' > "$FAKE_BIN"
chmod +x "$FAKE_BIN"

# A mock systemctl that always reports active, so health.sh's halt_banner does not render.
MOCK_SYSTEMCTL="$TMP/mock-systemctl"
printf '#!/bin/sh\necho active\n' > "$MOCK_SYSTEMCTL"
chmod +x "$MOCK_SYSTEMCTL"

# Sub-second tick for all three loops, and generous-but-small windows: the positive
# control always waits its whole window (timeout must expire to prove nothing exited
# early), the change case returns as soon as the loop notices — usually within one tick.
TICK=0.2
WIN_STAY=0.6
WIN_CHANGE=3
TOUCH_DELAY=0.3

run_health() {   # run_health <conf> <window>
    timeout "$2" env -i PATH="$BASE_PATH" HOME="$RUN" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$1" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t SPIRA_SYSTEMCTL="$MOCK_SYSTEMCTL" \
        SPIRA_HEALTH_TICK="$TICK" \
        bash "$COCKPIT_DIR/health.sh" loop
}

run_loom() {      # run_loom <conf> <window>
    timeout "$2" env -i PATH="$BASE_PATH" HOME="$RUN" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$1" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t SPIRA_LOOM_BIN="$FAKE_BIN" \
        SPIRA_LOOM_TICK="$TICK" \
        bash "$HERE/loom.sh"
}

run_collect() {   # run_collect <conf> <window>
    timeout "$2" env -i PATH="$BASE_PATH" HOME="$RUN" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$1" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t SPIRA_COCKPIT="$TMP" \
        SPIRA_COCKPIT_FORCE=1 SPIRA_COCKPIT_TICK="$TICK" \
        COCK="$FAKE_BIN" FRAG_DIR="$RUN/cockpit.d" \
        bash "$HERE/collect.sh" loop
}

for name in health loom collect; do
    case "$name" in
        health)  label="health.sh" ;;
        loom)    label="loom.sh" ;;
        collect) label="collect.sh" ;;
    esac

    RUN="$TMP/$name-run"; mkdir -p "$RUN/cockpit.d"

    # ── NEGATIVE CASE: no config change — the loop stays running ──────────────────────
    CONF_STAY="$TMP/$name-stay.conf"
    printf '# test\n' > "$CONF_STAY"
    ec_stay=0
    "run_$name" "$CONF_STAY" "$WIN_STAY" >/dev/null 2>/dev/null || ec_stay=$?
    is "$label: no config change — stays running" "124" "$ec_stay"

    # ── CHANGE CASE: touch the config file → the loop exits 0 ─────────────────────────
    CONF_CHG="$TMP/$name-chg.conf"
    printf '# test\n' > "$CONF_CHG"
    # Pre-date so the touch below produces a genuine mtime change, without needing to
    # wait out a real second of wall clock for the two stat reads to land apart.
    touch -d "3 seconds ago" "$CONF_CHG"
    ( sleep "$TOUCH_DELAY"; touch "$CONF_CHG" ) &
    touch_pid=$!
    ec_chg=0
    err_log="$TMP/$name-err.log"
    "run_$name" "$CONF_CHG" "$WIN_CHANGE" >/dev/null 2>"$err_log" || ec_chg=$?
    wait "$touch_pid" 2>/dev/null || true
    is   "$label: config change — exits 0"           "0" "$ec_chg"
    want "$label: config change — message on stderr" "config changed" "$(cat "$err_log" 2>/dev/null)"
done

tl_summary
