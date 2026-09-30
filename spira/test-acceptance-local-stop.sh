#!/usr/bin/env bash
# test-acceptance-local-stop.sh — acceptance-local.sh start/stop wiring: a run gets a NAME
# (spira-acc-<round>-<ts>, a systemd --user transient unit), and is stopped BY that name.
#
# THE INCIDENT THIS REGRESSES (sp-kb0k5). A Concierge subagent wanted to stop a leftover
# acceptance-local.sh. It read the process's PPid from /proc and killed it. The process was
# orphaned, so its PPid was the user manager itself; SIGTERM to it activated exit.target and
# the whole session — every aeon, sentinel, timer — stopped.
#
# WHAT THIS TESTS. The argument shape only — SPIRA_SYSTEMCTL/SPIRA_SUMMON are stubbed, so
# no real unit is ever created. That the stop is genuinely cgroup-wide (catches a child
# reparented before the stop runs) is the real dependency, proved for real, only on a host
# with a systemd --user session: test-acceptance-local-stop-live.sh.
#
# covers: spira/acceptance-local.sh spira/lib.sh
# tier: T1
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
SCRIPT="$HERE/acceptance-local.sh"

echo "test-acceptance-local-stop.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ===========================================================================
echo
echo "1. stop <round>: the glob is spira-acc-<round>-*, and every match is stopped by exact name"
# ===========================================================================
FAKE1="$TMP/fake-systemctl-1"
cat > "$FAKE1" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_LOG"
case " $* " in
    *' list-units '*)
        printf 'spira-acc-roundx-111.service loaded active running\n'
        printf 'spira-acc-roundx-222.service loaded active running\n'
        ;;
esac
exit 0
EOF
chmod +x "$FAKE1"
FAKE_LOG1="$TMP/fake1.log"; : > "$FAKE_LOG1"
_out1="$(FAKE_LOG="$FAKE_LOG1" SPIRA_SYSTEMCTL="$FAKE1" bash "$SCRIPT" stop roundx 2>&1)"; _rc1=$?
wantrc "stop exits 0 when every match stops" "0" "$_rc1"
want "list-units is asked for the round's own glob" "list-units spira-acc-roundx-*" "$(cat "$FAKE_LOG1")"
want "the first match is stopped by its exact name" "stop spira-acc-roundx-111.service" "$(cat "$FAKE_LOG1")"
want "the second match is stopped by its exact name" "stop spira-acc-roundx-222.service" "$(cat "$FAKE_LOG1")"
want "stop reports what it stopped" "stopped spira-acc-roundx-111.service" "$_out1"

# ===========================================================================
echo
echo "2. stop <round>: no match is not a failure — the run may already be finished"
# ===========================================================================
FAKE2="$TMP/fake-systemctl-2"
cat > "$FAKE2" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$FAKE2"
_out2="$(SPIRA_SYSTEMCTL="$FAKE2" bash "$SCRIPT" stop nomatch 2>&1)"; _rc2=$?
wantrc "stop exits 0 when nothing matches" "0" "$_rc2"
want "stop says no unit matched" "no unit matches spira-acc-nomatch-*" "$_out2"

# ===========================================================================
echo
echo "3. stop <round>: a match that fails to stop is reported and the exit is non-zero"
# ===========================================================================
FAKE3="$TMP/fake-systemctl-3"
cat > "$FAKE3" <<'EOF'
#!/usr/bin/env bash
case " $* " in
    *' list-units '*) printf 'spira-acc-failround-1.service loaded active running\n'; exit 0 ;;
    *' stop '*) exit 1 ;;
esac
exit 0
EOF
chmod +x "$FAKE3"
_out3="$(SPIRA_SYSTEMCTL="$FAKE3" bash "$SCRIPT" stop failround 2>&1)"; _rc3=$?
wantrc "stop exits 1 when a matched unit fails to stop" "1" "$_rc3"
want "the failure to stop is reported" "could not stop spira-acc-failround-1.service" "$_out3"

# ===========================================================================
echo
echo "4. start <round> <tree> [...]: launched under spira-acc-<round>-<ts>, argv forwarded"
# ===========================================================================
FAKE_SUMMON="$TMP/fake-summon"
cat > "$FAKE_SUMMON" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_SUMMON_LOG"
exit 0
EOF
chmod +x "$FAKE_SUMMON"
FAKE_SUMMON_LOG="$TMP/summon.log"; : > "$FAKE_SUMMON_LOG"
mkdir -p "$TMP/some-tree"
_start_out="$(FAKE_SUMMON_LOG="$FAKE_SUMMON_LOG" SPIRA_SUMMON="$FAKE_SUMMON" \
    bash "$SCRIPT" start myround "$TMP/some-tree" --predecessor spira-release-spira-x 2>&1)"
_start_rc=$?
wantrc "start exits 0 when the launch succeeds" "0" "$_start_rc"
_summon_argv="$(cat "$FAKE_SUMMON_LOG")"
want "the unit name follows spira-acc-<round>-<ts>" "--unit=spira-acc-myround-" "$_summon_argv"
want "the wrapped command is this script itself, with argv forwarded" \
    "-- $(command -v acceptance-local.sh) $TMP/some-tree --predecessor spira-release-spira-x" "$_summon_argv"
want "start reports the unit it launched" "started spira-acc-myround-" "$_start_out"
want "start tells the operator how to stop it" "stop myround" "$_start_out"

# ===========================================================================
echo
echo "5. the plain form (no start/stop) is unaffected — existing callers see no change"
# ===========================================================================
_rc5=0
bash "$SCRIPT" >/dev/null 2>&1 || _rc5=$?
wantrc "no argument still exits 2 (usage), never treated as a round name" "2" "$_rc5"

tl_summary
