#!/usr/bin/env bash
#
# test-ops-allowlist.sh — ops.fayth restricts Bash to a diagnostic allowlist;
#   writes and systemd mutations are refused by construction.
#
# WHAT IS PROVED
# ops.fayth FAYTH_TOOLS contains Bash(pattern) entries, not bare Bash. A command
# matching a diagnostic pattern is allowed; one that does not match any pattern is
# refused. The test is a unit check of the allowlist patterns against the real fayth
# file — a misconfigured allowlist (bare Bash, wrong patterns) fails before the
# Claude CLI is ever involved.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): diagnostic commands must
# be allowed, or the test fails — proving the guard does not merely refuse everything.
# A guard that refuses everything is indistinguishable from one that works until
# Ops sessions stop being able to do any work at all.
#
# SEEN RED on the prior FAYTH_TOOLS="Bash,Read,Glob,Grep,TodoWrite":
#   - "bare Bash absent" assertion fires (bare Bash IS present)
#   - diagnostic command assertions fail (no Bash(pattern) entries, matcher says "no")
#
# defect: sp-39pqw
# covers: spira/chamber/ops.fayth
# host-reason: hermetic — sources ops.fayth in a subshell via fayth_get; no database,
#   no systemd, no Claude invocations
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }

echo "test-ops-allowlist.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# Minimal environment: SPIRA_CONF at a nonexistent path so no host config leaks
# verdicts into the suite (law-gates-run-in-a-clean-environment). SPIRA_RUN at a
# temp dir so capacity_paused has no pause file.
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_SCOPE_LABEL="spira"
export SPIRA_INCIDENT_LABEL="incident"
export SPIRA_ASK_LABEL="needs-operator"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

ops_tools="$(fayth_get ops FAYTH_TOOLS)"

# ops_allows <cmd>: returns 0 if cmd matches any Bash(pattern) in ops FAYTH_TOOLS.
# Splits FAYTH_TOOLS by comma, strips Bash(...) wrapper, tests with bash glob.
ops_allows() {
    local cmd="$1"
    local IFS=","
    for entry in $ops_tools; do
        case "$entry" in
            Bash\(*)
                local pat="${entry#Bash(}"
                pat="${pat%)}"
                # shellcheck disable=SC2254
                [[ "$cmd" == $pat ]] && return 0
                ;;
        esac
    done
    return 1
}

# ==========================================================================================
echo
echo "ops.fayth — Bash is restricted (no bare Bash in FAYTH_TOOLS)"
# ==========================================================================================
case ",$ops_tools," in
    *",Bash,"*) bad "bare Bash absent from ops FAYTH_TOOLS" \
                    "found bare 'Bash' in: $ops_tools" ;;
    *)          ok  "bare Bash absent from ops FAYTH_TOOLS" ;;
esac

case "$ops_tools" in
    *"Bash("*) ok "at least one Bash(pattern) entry present" ;;
    *)         bad "no Bash(pattern) entries" "FAYTH_TOOLS: $ops_tools" ;;
esac

# ==========================================================================================
echo
echo "POSITIVE CONTROL — diagnostic commands are allowed"
# ==========================================================================================
# A guard that refuses everything looks identical to one that works. Each assertion here
# proves the allowlist lets Ops do its job; the write assertions below are only meaningful
# once these pass.
ops_allows "systemctl status spira-aeon-builder.service" \
    && ok  "systemctl status allowed" \
    || bad "systemctl status NOT allowed" "Ops cannot read service state"

ops_allows "systemctl show spira-sentinel.service" \
    && ok  "systemctl show allowed" \
    || bad "systemctl show NOT allowed" "Ops cannot inspect service properties"

ops_allows "journalctl -u spira-sentinel.service --since '1 hour ago'" \
    && ok  "journalctl allowed" \
    || bad "journalctl NOT allowed" "Ops cannot read journals"

ops_allows "cat /var/log/spira/ops.log" \
    && ok  "cat allowed" \
    || bad "cat NOT allowed" "Ops cannot read log files"

ops_allows "grep -r 'error' /var/log/spira/" \
    && ok  "grep allowed" \
    || bad "grep NOT allowed" "Ops cannot search logs"

ops_allows "ls -la /home/ryan/spira/run/worktree" \
    && ok  "ls allowed" \
    || bad "ls NOT allowed" "Ops cannot list directories"

ops_allows "bd -C /home/ryan/spira/db show sp-xyz" \
    && ok  "bd show allowed" \
    || bad "bd show NOT allowed" "Ops cannot read beads"

ops_allows "bd -C /home/ryan/spira/db note sp-xyz 'diagnosis'" \
    && ok  "bd note allowed" \
    || bad "bd note NOT allowed" "Ops cannot write diagnostic notes"

ops_allows "git log --oneline -10" \
    && ok  "git log allowed" \
    || bad "git log NOT allowed" "Ops cannot read git history"

ops_allows "git -C /home/ryan/spira/harness log --oneline -5" \
    && ok  "git -C ... log allowed" \
    || bad "git -C ... log NOT allowed" "Ops cannot read git history with -C"

ops_allows "git status" \
    && ok  "git status allowed" \
    || bad "git status NOT allowed" "Ops cannot check working tree status"

# ==========================================================================================
echo
echo "writes and mutations are refused"
# ==========================================================================================
ops_allows "rm -rf /home/ryan/spira/run/worktree" \
    && bad "rm refused" "rm was allowed — must not be" \
    || ok  "rm refused"

ops_allows "systemctl restart spira-aeon-builder.service" \
    && bad "systemctl restart refused" "systemctl restart was allowed — must not be" \
    || ok  "systemctl restart refused"

ops_allows "systemctl start spira-sentinel.service" \
    && bad "systemctl start refused" "systemctl start was allowed — must not be" \
    || ok  "systemctl start refused"

ops_allows "systemctl stop spira-aeon-builder.service" \
    && bad "systemctl stop refused" "systemctl stop was allowed — must not be" \
    || ok  "systemctl stop refused"

ops_allows "git branch -D spira/sp-xyz" \
    && bad "git branch -D refused" "git branch -D was allowed — must not be" \
    || ok  "git branch -D refused"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
