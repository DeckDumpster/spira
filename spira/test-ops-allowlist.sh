#!/usr/bin/env bash
#
# test-ops-allowlist.sh — restricted-persona tool allowlists: Bash is a diagnostic
#   allowlist, never bare, for every persona that is not supposed to have the run of the
#   place. Ops and czar today; a third restricted persona joins this table, not a new file.
#
# WHAT IS PROVED
# Each restricted persona's RUNE_TOOLS contains Bash(pattern) entries, not bare Bash. A
# command matching a diagnostic pattern is allowed; one that does not match any pattern is
# refused. The test is a unit check of the allowlist patterns against the real rune file —
# a misconfigured allowlist (bare Bash, wrong patterns) fails before the Claude CLI is ever
# involved.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): diagnostic commands must
# be allowed, or the test fails — proving the guard does not merely refuse everything.
# A guard that refuses everything is indistinguishable from one that works until
# the persona stops being able to do any work at all.
#
# SEEN RED on the prior RUNE_TOOLS="Bash,Read,Glob,Grep,TodoWrite":
#   - "bare Bash absent" assertion fires (bare Bash IS present)
#   - diagnostic command assertions fail (no Bash(pattern) entries, matcher says "no")
#
# TI GAP (UC-safety-fences-31, gap 11), NOT CLOSED HERE. `rune_allows` below is a
# test-local reimplementation of the Claude CLI's own `Bash(pattern)` matcher — the CLI does
# not expose that matcher for scripting, so a CLI that matched differently would pass this
# suite while the real fence differed. What this suite CAN and does do is keep the reimplemented
# matcher a single shared function exercised once per restricted persona, rather than a second
# copy per persona; closing the gap itself needs the CLI to expose a contract, which does not
# exist today.
#
# defect: sp-39pqw
# tier: T1
# covers: spira/runes/ops.rune spira/runes/czar.rune UC-safety-fences-30 UC-safety-fences-31 UC-dispatch-24
# host-reason: hermetic — sources each rune in a subshell; no database, no systemd, no
#   Claude invocations
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-ops-allowlist.sh"

# ops.rune and czar.rune build RUNE_LABELS/RUNE_EXCLUDE_LABELS from these — under `set -u`
# an unset one aborts the source, not just the assignment, so it must be defined even though
# this suite reads neither field. Sourcing conf.sh for its defaults would work too, but that
# is exactly the weight UC-safety-fences-30 asks this suite to stop paying.
export SPIRA_SCOPE_LABEL=spira SPIRA_INCIDENT_LABEL=incident SPIRA_ASK_LABEL=needs-operator \
       SPIRA_CI_LABEL=awaiting-ci SPIRA_CZAR_LABEL=czar-trigger

# rune_tools <persona> -> its RUNE_TOOLS field, read from the rune ALONE — the same
# subshell-source lib.sh's own rune_get uses (lib.sh:856), but without paying to source all
# 5,000-odd lines of lib.sh just to read one variable out of one file (UC-safety-fences-30).
rune_tools() {
    local f="$HERE/runes/$1.rune"
    [ -f "$f" ] || return 1
    # shellcheck disable=SC1090
    ( . "$f" 2>/dev/null; printf '%s' "${RUNE_TOOLS:-}" )
}

# rune_allows <tools-string> <cmd>: true if cmd matches any Bash(pattern) entry.
# Splits on comma, strips the Bash(...) wrapper, tests with a bash glob.
rune_allows() {
    local tools="$1" cmd="$2" entry pat
    local IFS=","
    for entry in $tools; do
        case "$entry" in
            Bash\(*)
                pat="${entry#Bash(}"; pat="${pat%)}"
                # shellcheck disable=SC2254
                [[ "$cmd" == $pat ]] && return 0
                ;;
        esac
    done
    return 1
}

# persona_table <persona> <tools> — the shared shape every restricted persona is put through:
# no bare Bash, at least one Bash(pattern) entry, its diagnostics allowed (positive control
# first), its mutations refused.
persona_table() {
    local persona="$1" tools="$2"

    echo
    echo "$persona.rune — Bash is restricted (no bare Bash in RUNE_TOOLS)"
    case ",$tools," in
        *",Bash,"*) bad "$persona: bare Bash absent from RUNE_TOOLS" \
                        "found bare 'Bash' in: $tools" ;;
        *)          ok  "$persona: bare Bash absent from RUNE_TOOLS" ;;
    esac
    case "$tools" in
        *"Bash("*) ok "$persona: at least one Bash(pattern) entry present" ;;
        *)         bad "$persona: no Bash(pattern) entries" "RUNE_TOOLS: $tools" ;;
    esac

    echo
    echo "$persona — POSITIVE CONTROL: diagnostic commands are allowed"
    local row cmd label
    while IFS='|' read -r cmd label; do
        [ -n "$cmd" ] || continue
        rune_allows "$tools" "$cmd" \
            && ok  "$persona: $label allowed" \
            || bad "$persona: $label NOT allowed" "command: $cmd"
    done <<EOF_ALLOW
$3
EOF_ALLOW

    echo
    echo "$persona — writes and mutations are refused"
    while IFS='|' read -r cmd label; do
        [ -n "$cmd" ] || continue
        rune_allows "$tools" "$cmd" \
            && bad "$persona: $label refused" "$label was allowed — must not be" \
            || ok  "$persona: $label refused"
    done <<EOF_DENY
$4
EOF_DENY
}

OPS_TOOLS="$(rune_tools ops)"
[ -n "$OPS_TOOLS" ] || bail "ops.rune not found or RUNE_TOOLS unset"

OPS_ALLOW="systemctl status spira-dwarf-builder.service|systemctl status
systemctl show spira-sentinel.service|systemctl show
cat /var/log/spira/ops.log|cat
grep -r 'error' /var/log/spira/|grep
ls -la /srv/spira/run/worktree|ls
git log --oneline -10|git log
git -C /srv/spira/harness log --oneline -5|git -C ... log
git status|git status"

OPS_DENY="bd -C /srv/spira/db show sp-xyz|bd show NOT granted (dwarves reach the store only through work)
bd -C /srv/spira/db note sp-xyz 'diagnosis'|bd note NOT granted (dwarves reach the store only through work)
rm -rf /srv/spira/run/worktree|rm
systemctl restart spira-dwarf-builder.service|systemctl restart
systemctl start spira-sentinel.service|systemctl start
systemctl stop spira-dwarf-builder.service|systemctl stop
git branch -D spira/sp-xyz|git branch -D"

persona_table ops "$OPS_TOOLS" "$OPS_ALLOW" "$OPS_DENY"

# ==========================================================================================
# czar.rune — the second restricted persona (UC-safety-fences-30's extension, gap 11).
# czar carries its own destructive-vocabulary surface (queue eject, landing halt), so its
# allow rows exercise those instead of Ops's systemctl diagnostics.
# ==========================================================================================
CZAR_TOOLS="$(rune_tools czar)"
[ -n "$CZAR_TOOLS" ] || bail "czar.rune not found or RUNE_TOOLS unset"

CZAR_ALLOW="bash spira/czar-fence.sh deadlock|czar-fence.sh
/srv/spira/bin/queue eject sp-xyz|queue eject
/srv/spira/bin/landing-pass halt --reason-file -|landing-pass halt
git log --oneline -10|git log
cat /var/log/spira/czar.log|cat
ls -la /srv/spira/run/worktree|ls"

CZAR_DENY="bd -C /srv/spira/db show sp-xyz|bd show NOT granted (dwarves reach the store only through work)
bd -C /srv/spira/db note sp-xyz 'diagnosis'|bd note NOT granted (dwarves reach the store only through work)
rm -rf /srv/spira/run/worktree|rm
systemctl restart spira-sentinel.service|systemctl restart
systemctl start spira-sentinel.service|systemctl start
systemctl stop spira-sentinel.service|systemctl stop
git branch -D spira/sp-xyz|git branch -D"

persona_table czar "$CZAR_TOOLS" "$CZAR_ALLOW" "$CZAR_DENY"

tl_summary
