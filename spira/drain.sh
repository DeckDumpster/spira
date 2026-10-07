#!/usr/bin/env bash
#
# drain.sh — is Gas Town actually draining, and may it be retired yet?
#
#   drain.sh            the measurement
#   drain.sh --verdict  exit 0 only if it is safe to retire
#
# WHY A NUMBER AND NOT A FEELING. Retirement needs evidence: open work in every Gas Town
# database, agents that are genuinely alive, and PRs still in flight. law-freeze-gastown-
# dispatch stopped NEW work; that is not the same as the old work finishing, and a freeze
# that does not drain is a stall wearing a freeze's clothes.
#
# LIVENESS IS `gt agents`, NEVER `gt polecat list` or `gt rig status` — those count
# DIRECTORIES, so a reaped rig still reports its polecats. And `gt` reports an empty town
# unless the cwd is inside it.
set -uo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/lib.sh"
TOWN="$SPIRA_TOWN"
SPIRA="$SPIRA_DB"
# A colleague has no Gas Town, and this whole report is about retiring one. Say so and stop,
# rather than printing a page of zeroes that reads like a clean drain.
if [ -z "$TOWN" ]; then
    printf 'drain: no predecessor harness configured (SPIRA_TOWN is empty in %s)\n' \
        "${SPIRA_CONF_FILE:-spira.conf}"
    exit 0
fi
VERDICT=0; [ "${1:-}" = "--verdict" ] && VERDICT=1

open_in() {   # open_in <db> -> count of non-closed beads, or ? if unreadable
    local db="$1" n
    # batch-job: lists every bead in a rig store while draining
    n=$(timeout 180 bd -C "$db" list --all --limit 0 --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("?"); raise SystemExit
rows = d if isinstance(d, list) else [d]
# A wisp is ephemeral by design and is not outstanding work.
print(sum(1 for i in rows
          if i.get("status") not in ("closed", "tombstone")
          and not str(i.get("id", "")).startswith(("hq-wisp", "wisp"))))
' 2>/dev/null) || n="?"
    printf '%s' "${n:-?}"
}

total=0; unknown=0
printf 'GAS TOWN — open work per database\n'
for db in "$TOWN" "$TOWN"/*/; do
    db="${db%/}"
    [ -d "$db/.beads" ] || continue
    [ "$(basename "$db")" = deacon ] && continue
    name="$(basename "$db")"; [ "$db" = "$TOWN" ] && name=town
    n="$(open_in "$db")"
    printf '  %-14s %s\n' "$name" "$n"
    case "$n" in ''|*[!0-9]*) unknown=$((unknown+1)) ;; *) total=$((total+n)) ;; esac
done
printf '  %-14s %s%s\n' TOTAL "$total" "$( [ "$unknown" -gt 0 ] && printf ' (+%s unreadable)' "$unknown" )"

printf '\nGAS TOWN — agents genuinely alive (gt agents, not directory counts)\n'
# `gt agents` PRINTS NO ● — that marker is `gt status`. Grepping for it here returned 0
# while 24 claude processes were running under Gas Town settings, so the drain reported a
# fully populated town as empty and would have called it safe to retire. The documented
# liveness source is `gt status --json` -> .rigs[].agents[].running (CLAUDE.md), and /proc
# is the cross-check that owes nothing to either command's formatting.
# batch-job: town status across every rig while draining
alive=$( (cd "$TOWN" && timeout 240 gt status --json 2>/dev/null) | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: print("?"); raise SystemExit
n = 0
for r in d.get("rigs", []) or []:
    for a in r.get("agents", []) or []:
        if a.get("running"): n += 1
for k in ("mayor", "deacon"):
    a = d.get(k) or {}
    if isinstance(a, dict) and a.get("running"): n += 1
print(n)
' 2>/dev/null) || alive="?"
[ -n "$alive" ] || alive="?"
# The cross-check: claude processes whose command line names a Gas Town settings file.
# argv from /proc, never pgrep -f, whose pattern would match this script's own line.
procs=0
for d in /proc/[0-9]*; do
    [ -r "$d/cmdline" ] || continue
    case "$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null)" in
        # MATCH THE AGENT, NOT ANYTHING THAT MENTIONS THE PATH. The first pattern caught
        # this script's own caller — a `/bin/bash -c` whose command line contains
        # the predecessor town because it exports gt's bin on PATH — and reported 2 phantom Gas
        # Town agents after the town was fully stopped. That is the pgrep -f trap with a
        # different tool: the pattern is a substring of the searcher's own command line.
        # A real agent is the claude binary carrying a Gas Town settings.json.
        *claude*--settings\ "$TOWN"/*settings.json*) procs=$((procs+1)) ;;
    esac
done
printf '  gt status     %s\n  claude procs  %s\n' "${alive:-?}" "$procs"
# Disagreement between the two is itself the finding: one of them is measuring the wrong
# thing, and a retirement decision must not be taken on either until they agree.
if [ "$alive" != "?" ] && [ "${alive:-0}" -eq 0 ] && [ "$procs" -gt 0 ]; then
    printf '  MISMATCH      gt reports 0 agents while %s claude processes run under it\n' "$procs"
fi

printf '\nGAS TOWN — work still in flight outside beads\n'
# batch-job: merge-queue listing across the town while draining
mq=$( (cd "$TOWN" && timeout 180 gt mq 2>/dev/null) | grep -cE '^\s*[a-z]{2}-' || true )
printf '  merge queue   %s\n' "${mq:-?}"
prs=0
# THE REPOSITORIES COME FROM THE MAP, not from a list written here. A list in the code is
# one operator's inventory, and it goes stale the first time a repository is added.
for r in $(repo_names); do
    rp="$(repo_root "$r" 2>/dev/null)" || continue
    [ -d "$rp/.git" ] || continue
    # `gh --json` emits the whole array on ONE line, so `grep -c` counts lines and reported
    # three PRs as one. Ask gh for the length rather than counting text.
    # ONLY GAS TOWN'S PRs BLOCK GAS TOWN'S RETIREMENT. Counting every open PR meant
    # Spira's own work — which opens PRs in these same repos, in pr land mode — would
    # register as a blocker, so the verdict could never go clear no matter how much of
    # Gas Town was gone. A polecat branch is Gas Town's; spira/* is ours.
    # batch-job: lists open PRs per repo while draining
    n=$( (cd "$rp" && timeout 120 gh pr list --state open --json headRefName \
            -q '[.[] | select(.headRefName | startswith("polecat/"))] | length' 2>/dev/null) | tail -1 )
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    # batch-job: lists open PRs per repo while draining
    mine=$( (cd "$rp" && timeout 120 gh pr list --state open --json headRefName \
            -q '[.[] | select(.headRefName | startswith("spira/"))] | length' 2>/dev/null) | tail -1 )
    case "$mine" in ''|*[!0-9]*) mine=0 ;; esac
    [ "${mine:-0}" -gt 0 ] && printf '  spira PRs %-5s %s (not a blocker)\n' "$r" "$mine"
    [ "${n:-0}" -gt 0 ] && printf '  polecat PRs %-5s %s\n' "$r" "$n"
    prs=$((prs + ${n:-0}))
done
printf '  polecat PRs   %s\n' "$prs"

printf '\nSPIRA — what is taking over\n'
sp_open="$(open_in "$SPIRA")"
# READY_ARGS (lib.sh), not a copy: a readout of "what Spira can take" that counts beads no
# aeon can claim is the number that made the queue look healthy while it starved.
# batch-job: full ready listing while draining
sp_plan=$(timeout 180 bd -C "$SPIRA" "${READY_ARGS[@]}" --label "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan" \
            --exclude-label "spira-poison,$SPIRA_ASK_LABEL" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
          | python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: print("?"); raise SystemExit
print(len(d if isinstance(d,list) else [d]))' 2>/dev/null)
landed=$(git -C "$(repo_root)" log --since=24.hours --format='%an' 2>/dev/null | grep -c '^aeon-' || true)
printf '  open beads    %s\n  plan ready    %s\n  landed by aeons in 24h  %s\n' "$sp_open" "${sp_plan:-?}" "${landed:-0}"

printf '\nVERDICT\n'
blockers=0
[ "${alive:-1}" -gt 0 ] 2>/dev/null && { printf '  BLOCKED  %s agent(s) still running per gt status\n' "$alive"; blockers=$((blockers+1)); }
[ "$procs" -gt 0 ] && { printf '  BLOCKED  %s claude process(es) still running under Gas Town\n' "$procs"; blockers=$((blockers+1)); }
[ "$prs" -gt 0 ]        2>/dev/null && { printf '  BLOCKED  %s polecat PR(s) — Gas Town work with no author left\n' "$prs"; blockers=$((blockers+1)); }
[ "${mq:-0}" -gt 0 ]    2>/dev/null && { printf '  BLOCKED  %s item(s) in the merge queue\n' "$mq"; blockers=$((blockers+1)); }
[ "$unknown" -gt 0 ]    2>/dev/null && { printf '  BLOCKED  %s database(s) unreadable — a probe that fails is not a clear queue\n' "$unknown"; blockers=$((blockers+1)); }
if [ "$blockers" -eq 0 ]; then
    printf '  CLEAR    no live agents, no open PRs, no queued merges.\n'
    printf '           %s open beads remain in the databases; they are mirrored in Spira and\n' "$total"
    printf '           are records, not running work. Safe to retire.\n'
fi
[ "$VERDICT" = 1 ] && exit "$blockers"
exit 0
