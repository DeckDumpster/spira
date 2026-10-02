#!/usr/bin/env bash
#
# test-gate-wait.sh — sp-tcarr: a waiter keyed on the `gate: VERDICT=` marker can wait
# forever, because a gate killed by SIGKILL mid-run writes nothing. `gate wait <out>`
# exists so a caller blocks on the run's own pid instead of the marker:
#
#   1. kill -9 a gate mid-run: `gate wait <out>` returns NO_VERDICT within seconds.
#   2. kill -TERM a gate mid-run: the output still ends with a `gate: VERDICT=` line
#      (already true before this bead — TERM/INT/HUP are caught, real.rs) — the negative
#      control that makes case 1 mean something: if even TERM left nothing, `wait` would
#      be covering for a bug one layer down instead of the one SIGKILL actually has.
#
# tier: T2
# covers: gate/src/wait.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TOOLS="$(dirname "$(command -v spira-config)"):$HERE"
. "$HERE/testlib/gate-fixture.sh"

command -v setsid >/dev/null 2>&1 || { echo "  SKIP  setsid is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
gate_fixture_init "$TMP"
BR=spira/sp-w1
gate_fixture_branch "$BR"

echo "test-gate-wait.sh — sp-tcarr"

run_killed() {
    # Runs the fixture's gate.sh against a fixture command that sleeps, in its own
    # session (so a signal reaches the gate command underneath gate.sh, exactly as
    # test-gate-metering.sh's UC-gate-verdict-25 case does), and sends $1 once the
    # command is confirmed running. Writes combined output to $OUT and the gate binary's
    # own pid (after gate.sh's `exec -a` replaces it) to $OUT.pid.
    local sig="$1" mark="$TMP/mark-$sig" out="$TMP/out-$sig" pidfile="$TMP/gate-$sig.pid"
    printf 'repo | %s | push | origin/main |  | rm -f %s; : > %s; sleep 30\n' \
        "$REPO" "$mark" "$mark" > "$MAP"
    (
        export HOMEDIR SPIRA_CONF_NONE REPO RUN SPIRA_DB_NONE MAP GATELOG VDIR SH BR pidfile TOOLS
        setsid bash -c '
            echo "$$" > "$pidfile"
            exec env -i SPIRA_RELEASE="$SPIRA_RELEASE" HOME="$HOMEDIR" PATH="$SH:$TOOLS:/usr/bin:/bin" \
                GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
                SPIRA_CONF="$SPIRA_CONF_NONE" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
                SPIRA_DB="$SPIRA_DB_NONE" SPIRA_REPO_MAP="$MAP" \
                SPIRA_GATE_LOG="$GATELOG" SPIRA_VERDICT_TTL=0 \
                bash "$SH/gate.sh" "$BR" repo
        '
    ) >"$out" 2>&1 &
    local bgpid=$!

    for _ in $(seq 1 100); do [ -s "$pidfile" ] && [ -f "$mark" ] && break; sleep 0.1; done
    local gatepid; gatepid="$(cat "$pidfile" 2>/dev/null || true)"
    if [ -z "$gatepid" ] || [ ! -f "$mark" ]; then
        bad "gate command reached mid-run before the signal ($sig)" "pid=[${gatepid:-}] mark=$([ -f "$mark" ] && echo yes || echo no)"
        return 1
    fi
    cp "$pidfile" "$out.pid"
    case "$sig" in
        9)    kill -9 "$gatepid" 2>/dev/null || true ;;
        TERM) kill -TERM "-$gatepid" 2>/dev/null || true ;;
    esac
    wait "$bgpid" 2>/dev/null || true
    printf '%s' "$out"
}

# ------------------------------------------------------------------------------------
# CASE 1 — SIGKILL. The gate process is gone with no VERDICT line (nothing can run
# after SIGKILL); `gate wait` must still terminate, fast, and say NO_VERDICT.
# ------------------------------------------------------------------------------------
out1="$(run_killed 9)"
if [ -n "$out1" ]; then
    nowant "a SIGKILLed run writes no gate: VERDICT= line" "gate: VERDICT=" "$(cat "$out1")"
    t0=$(date +%s)
    wait_out="$(gate wait "$out1" 2>&1)"; wait_rc=$?
    elapsed=$(( $(date +%s) - t0 ))
    [ "$elapsed" -le 10 ] \
        && ok "gate wait returned within seconds of the SIGKILL (${elapsed}s)" \
        || bad "gate wait latency" "took ${elapsed}s"
    is  "gate wait's exit code is NO_VERDICT (75)" 75 "$wait_rc"
    want "gate wait's own line says NO_VERDICT" "VERDICT=NO_VERDICT" "$wait_out"
    want "and names why: gone"                  "reason=gone"        "$wait_out"
fi

# ------------------------------------------------------------------------------------
# CASE 2 — SIGTERM, the negative control. Already caught (real.rs install_signal_handlers,
# UC-gate-verdict-25): the run still meters, tears down and prints its own VERDICT line
# before exiting, so the output itself (not just `gate wait`) carries NO_VERDICT.
# ------------------------------------------------------------------------------------
out2="$(run_killed TERM)"
if [ -n "$out2" ]; then
    last_line="$(tail -1 "$out2")"
    case "$last_line" in
        *"gate: VERDICT=NO_VERDICT"*) ok "a SIGTERMed run's output ends with gate: VERDICT=NO_VERDICT" ;;
        *) bad "SIGTERM output" "last line: $last_line" ;;
    esac
fi

tl_summary
