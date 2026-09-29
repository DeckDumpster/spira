#!/usr/bin/env bash
#
# test-conf-watch.sh — health.sh, loom.sh and collect.sh all notice, once, when SPIRA_CONF's
# file changes under them (law-long-lived-processes-pin-their-config).
#
# ONE TABLE OVER THE THREE LOOPS: each loop's tick is injected — SPIRA_HEALTH_TICK,
# SPIRA_LOOM_TICK, SPIRA_COCKPIT_TICK — through the shared `conf_changed` helper in conf.sh,
# so both the positive control and the change case run in well under a second per script.
#
# loom.sh and collect.sh run under systemd (Restart=always) and exit 0 for the supervisor to
# restart them. health.sh IS its tmux pane, with no supervisor — exiting would close the pane
# and take its @cockpit tag with it (sp-94yqa), so it re-execs itself in place instead and
# never voluntarily exits; `timeout` is what ends it here.
#
# WHAT IS EXERCISED, per script:
#   1. NEGATIVE CASE (positive control): the loop stays running when the config is unchanged.
#   2. CHANGE CASE: each logs "config changed" when the config's mtime moves; loom.sh and
#      collect.sh then exit 0, while health.sh keeps running (timeout reaps it instead).
#
# ROOT CAUSE (sp-vlxxl) OF THE round-93b FLIP: the config file used to be touched by a
# background `sleep <fixed delay>; touch`, racing each script's own startup — sourcing conf.sh
# and capturing `_conf_mtime_0` — under a maxpar-16 corpus. When the touch landed first, the
# captured baseline was already the post-touch mtime, `conf_changed` never fired, and the
# process ran out its whole window silently: the ONLY stderr line then was conf.sh's own
# (harmless, pre-existing) auto-convert-failed notice from a fixture with no spira-config
# binary, with no "config changed" ever printed. The fix removes the wall-clock delay
# entirely: each script already produces an observable side effect strictly AFTER its own
# `_conf_mtime_0` capture — health.sh calls SPIRA_SYSTEMCTL from its first `paint`, loom.sh and
# collect.sh spawn $COCK/$SPIRA_LOOM_BIN — so the mocks for those touch an "armed" marker, and
# the config file is only ever touched once that marker exists (law-a-retry-must-change-an-
# input's sibling: wait on a signal, not a clock).
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

# A fake binary that arms, then loops until killed: stands in for the Loom server binary
# (loom.sh) and for the probe subcommand (collect.sh) alike — both loops only need a
# long-lived child, and both start it strictly after capturing their own conf-mtime baseline.
FAKE_BIN="$TMP/fake-bin"
printf '#!/bin/sh\ntouch "$ARMED_MARKER" 2>/dev/null\nsleep 30\n' > "$FAKE_BIN"
chmod +x "$FAKE_BIN"

# A mock systemctl that always reports active (so health.sh's halt_banner does not render)
# and, on the way, arms the marker — health.sh's first `paint` calls it strictly after
# capturing its own conf-mtime baseline, once per tick thereafter.
MOCK_SYSTEMCTL="$TMP/mock-systemctl"
printf '#!/bin/sh\ntouch "$ARMED_MARKER" 2>/dev/null\necho active\n' > "$MOCK_SYSTEMCTL"
chmod +x "$MOCK_SYSTEMCTL"

# Sub-second tick for all three loops, and generous-but-small windows: the positive
# control always waits its whole window (timeout must expire to prove nothing exited
# early), the change case returns as soon as the loop notices — usually within one tick.
TICK=0.2
WIN_STAY=0.6
# 15, not 3: a loop's startup (sourcing conf.sh and lib.sh) took past 3s under a 16-wide
# corpus (2026-09-26, rc=124 "Terminated"). The exiting loops return the moment they notice
# the change, so the window costs time only on failure and for health.sh's reaped re-exec.
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

run_health() {   # run_health <conf> <window> <marker>
    timeout "$2" env -i PATH="$BASE_PATH" HOME="$RUN" TERM=dumb LC_ALL=C.UTF-8 \
        SPIRA_CONF="$1" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t SPIRA_SYSTEMCTL="$MOCK_SYSTEMCTL" \
        ARMED_MARKER="${3:-}" \
        SPIRA_HEALTH_TICK="$TICK" \
        bash "$COCKPIT_DIR/health.sh" loop
}

run_loom() {      # run_loom <conf> <window> <marker>
    timeout "$2" env -i PATH="$BASE_PATH" HOME="$RUN" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$1" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t SPIRA_LOOM_BIN="$FAKE_BIN" \
        ARMED_MARKER="${3:-}" \
        SPIRA_LOOM_TICK="$TICK" \
        bash "$HERE/loom.sh"
}

run_collect() {   # run_collect <conf> <window> <marker>
    timeout "$2" env -i PATH="$BASE_PATH" HOME="$RUN" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$1" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_RUN="$RUN" SPIRA_DB="$TMP/nodb" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t SPIRA_COCKPIT="$TMP" \
        SPIRA_COCKPIT_FORCE=1 SPIRA_COCKPIT_TICK="$TICK" \
        ARMED_MARKER="${3:-}" \
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

    # ── CHANGE CASE: touch the config only once the loop has armed ────────────────────
    CONF_CHG="$TMP/$name-chg.conf"
    printf '# test\n' > "$CONF_CHG"
    # Pre-date so the touch below produces a genuine mtime change even at 1s
    # resolution, without needing to wait out a real second of wall clock.
    touch -d "3 seconds ago" "$CONF_CHG"
    marker="$RUN/armed"
    err_log="$TMP/$name-err.log"
    "run_$name" "$CONF_CHG" "$WIN_CHANGE" "$marker" >/dev/null 2>"$err_log" &
    run_pid=$!
    if wait_for_marker "$marker"; then
        ok "$label: config change — armed before the touch"
    else
        bad "$label: config change — armed before the touch" "marker never appeared within 10s"
    fi
    touch "$CONF_CHG"
    ec_chg=0
    wait "$run_pid" || ec_chg=$?
    if [ "$name" = health ]; then
        # No supervisor restarts a tmux pane, so health.sh re-execs itself instead of
        # exiting; it never returns control here, and $WIN_CHANGE's `timeout` reaps it.
        is "$label: config change — re-execs, timeout reaps it" "124" "$ec_chg"
    else
        is "$label: config change — exits 0" "0" "$ec_chg"
    fi
    want "$label: config change — message on stderr" "config changed" "$(cat "$err_log" 2>/dev/null)"
done

tl_summary
