#!/usr/bin/env bash
#
# test-gate-metering.sh — every verdict reached after the tree lock writes exactly one
# gate.log row; a preflight refusal, reached before the meter exists, writes none.
#
#   ./test-gate-metering.sh
#
# docs/test-plan/gate-verdict.md gap #12 (UC-gate-verdict-25). gate.sh defines gate_meter()
# only after the repo-map, the diff and the universal layer have all already had their
# chance to refuse — a row for a run the meter never saw would say the opposite of what
# happened. verdict() calls gate_meter directly and disarms the EXIT trap before it exits,
# so a normal way out is metered once, by verdict() itself; the trap is the backstop for a
# death nobody chose, and it must still meter exactly once, not zero and not twice.
#
# tier: T2
# covers: spira/gate.sh UC-gate-verdict-25
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"

command -v flock  >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH";  exit 77; }
command -v setsid >/dev/null 2>&1 || { echo "  SKIP  setsid is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
gate_fixture_init "$TMP"
BR=spira/sp-m1
gate_fixture_branch "$BR"

meterlines() { [ -f "$GATELOG" ] && wc -l < "$GATELOG" | tr -d ' ' || echo 0; }

echo "test-gate-metering.sh — UC-gate-verdict-25"

# --------------------------------------------------------------------------------------
# POSITIVE CONTROL. A verdict reached after the lock writes exactly one row — proves the
# meter is wired at all before the refusal case below is trusted to mean anything.
# --------------------------------------------------------------------------------------
rm -f "$GATELOG"
out="$(gate_fixture_run "$BR" repo)"; rc=$?
is "a normal pass writes exactly one meter row" 1 "$(meterlines)"
is "and it passed"                              0 "$rc"

# --------------------------------------------------------------------------------------
# A PREFLIGHT REFUSAL, reached before GATE_LOG/gate_meter are even defined, writes none.
# --------------------------------------------------------------------------------------
rm -f "$GATELOG"
out="$(gate_fixture_run "$BR" repo SPIRA_REPO_MAP="$TMP/no-such-map")"; rc=$?
is "a preflight refusal exits NO_VERDICT"        75 "$rc"
is "and a preflight refusal writes no meter row"  0 "$(meterlines)"

# --------------------------------------------------------------------------------------
# A GATE KILLED MID-RUN IS STILL METERED, EXACTLY ONCE — by the backstop trap, since a
# SIGTERM sent to gate.sh itself never reaches verdict(). The whole invocation runs in its
# own session (setsid), so the signal is sent to the process GROUP: it reaches the gate
# command sleeping underneath gate.sh at the same instant it reaches gate.sh's own process,
# rather than gate.sh's wait() sitting on the foreground job for the trap-deferral window
# bash otherwise applies while a signal is pending against a running foreground command.
# --------------------------------------------------------------------------------------
rm -f "$GATELOG"
MARK="$TMP/mid-run"; PIDFILE="$TMP/gate.pid"
printf 'repo | %s | push | origin/main |  | rm -f %s; : > %s; sleep 30\n' \
    "$REPO" "$MARK" "$MARK" > "$MAP"
(
    export HOMEDIR SPIRA_CONF_NONE REPO RUN SPIRA_DB_NONE MAP GATELOG VDIR SH BR PIDFILE
    setsid bash -c '
        echo "$$" > "$PIDFILE"
        exec env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
            GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
            SPIRA_CONF="$SPIRA_CONF_NONE" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
            SPIRA_DB="$SPIRA_DB_NONE" SPIRA_REPO_MAP="$MAP" \
            SPIRA_GATE_LOG="$GATELOG" SPIRA_VERDICTS="$VDIR" SPIRA_VERDICT_TTL=0 \
            bash "$SH/gate.sh" "$BR" repo
    '
) >/dev/null 2>&1 &

for _ in $(seq 1 100); do [ -s "$PIDFILE" ] && [ -f "$MARK" ] && break; sleep 0.1; done
GATEPID="$(cat "$PIDFILE" 2>/dev/null || true)"
if [ -n "$GATEPID" ] && [ -f "$MARK" ]; then
    kill -TERM "-$GATEPID" 2>/dev/null || true
else
    bad "gate command reached mid-run before the kill" "pid=[${GATEPID:-}] mark present=$([ -f "$MARK" ] && echo yes || echo no)"
fi
wait 2>/dev/null || true
for _ in $(seq 1 50); do [ -s "$GATELOG" ] && break; sleep 0.1; done
is "a gate killed mid-run writes exactly one meter row" 1 "$(meterlines)"

tl_summary
