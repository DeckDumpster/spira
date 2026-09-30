#!/usr/bin/env bash
#
# pr-notify.sh — report PR transitions across every repo in the repo-map, whatever its land
# mode: opened, RED (with the failing check names), GREEN, merged, closed.
#
#   pr-notify.sh            one scan; same as --show
#   pr-notify.sh --show     one scan; prints to stdout
#   pr-notify.sh watch [--interval S] [--ticks N]   loop forever (or N times); watchd daemon
#   pr-notify.sh health      exit non-zero when the watch loop has stopped polling
#   pr-notify.sh actionable [FILE]   FILE (default: this watcher's own log) filtered down to
#                            what SPIRA_ACTIONABLE marks worth a look, for a repo still in the
#                            repo-map, with repeats of the same line collapsed to one
#
# WHY THIS EXISTS. law-green-prs-merge-themselves required visibility into which managed-repo
# PRs are ready to merge or have broken CI. That visibility was provided by gate-check; when
# gate-check was turned off, it went with it. This script restores it.
#
# EVERY REPO, NOT JUST land=pr. A repo-map row's land mode decides how a branch reaches its
# base, never whether a PR watching it is worth having: a queue-mode repo's batch PR is a real
# open PR with real CI, and one going red unreported is exactly the gap this closed
# (law-concierge-watches-every-registered-repo). Only the "branch with no PR" check stays
# scoped to land=pr — a push-mode repo merges directly (no PR ever exists) and a queue-mode
# branch not yet batched is not a stray, so reporting it there would be the noise this file
# exists to avoid.
#
# TRANSITIONS, NOT STATE. A PR unchanged since the last tick emits nothing — this is what
# makes a 90s cadence safe. Each PR's last known status lives in
# $SPIRA_RUN/watchd/pr-notify-state/<repo>.tsv; a PR seen for the first time gets OPENED (and,
# if its checks already resolved by the time it was first seen, the RED/GREEN line too); a PR
# that drops out of the open list gets MERGED or CLOSED, once, from `gh pr view`. A gh call
# that fails leaves the state untouched and is retried next tick rather than guessed at
# (law-a-control-that-cannot-check-must-refuse).
#
# GREEN NEVER NEEDS A HUMAN, WHATEVER THE LAND MODE. Every `pr`-mode PR has auto-merge armed
# the moment it opens and a stalled arm is watchtower's own escalation, not this watcher's
# (law-green-prs-merge-themselves); telling the operator to hand-merge one is not just noise,
# it is wrong twice over — the merge is already going to happen, and doing it by hand races
# the exact automation this watcher would be reporting on. RED keeps `FAIL` in every mode: a
# broken run is worth a look whatever gets it back to green.
#
# A PASS THAT FINDS NOTHING EMITS NOTHING. Silence is the healthy state, not a finding.
# A pass over repos with no gh access is silent, not an error: the check itself is running
# on behalf of the operator, who knows whether gh is configured.

# covers: spira/pr-notify.sh spira/watchers spira/world.sh systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/lib.sh"

STATE_DIR="$SPIRA_RUN/watchd/pr-notify-state"
HEALTH_FILE="$SPIRA_RUN/watchd/pr-notify.health"
LOG_FILE="$SPIRA_RUN/watchd/pr-notify.log"

_pr_state_file() { printf '%s/%s.tsv' "$STATE_DIR" "$1"; }

_emit() { printf '%s\n' "$1"; }

# _report <line> — a PR transition: printed (the watchd log, for history) AND mailed
# straight to the concierge mailbox as `--kind event`, never `note`: a machine event gets
# spira-mail-deliver's near-zero SPIRA_MAIL_SETTLE_EVENT, not the multi-minute
# SPIRA_MAIL_SETTLE window that batches the operator's OWN replies landing in the same
# mailbox. The log line alone depends on a session holding an in-session Monitor or the
# watch-notify escalation timer, either of which can be down or unattended; mailing here
# means delivery survives both, because it never depends on anything but this tick running.
_report() {
    local line="$1"
    _emit "$line"
    mail send concierge \
        --from "PR Notify <pr-notify@spira>" \
        --subject "pr-notify: $line" \
        --kind event <<MAILEOF >/dev/null || printf 'pr-notify: mail send failed for: %s\n' "$line" >&2
## Event
$line
MAILEOF
}

# _PR_STATUS_PY — Python3 script to classify every open PR from `gh pr list --json` output.
#
# Reads JSON from stdin, writes one tab-separated line per PR: number, status
# (pending|green|red), title, and — for red only — the comma-joined names of the failing
# checks. UNLIKE the old classifier this never skips a PR: a pending one still has to be
# tracked so its eventual OPENED/merge/close is seen.
_PR_STATUS_PY='
import json, sys
try:
    prs = json.load(sys.stdin)
except Exception:
    sys.exit(0)
BAD = {"FAILURE", "CANCELLED", "TIMED_OUT", "STALE", "ACTION_REQUIRED"}
for pr in prs:
    n = pr.get("number", "?")
    title = (pr.get("title") or "").replace("\n", " ").replace("\t", " ")
    checks = pr.get("statusCheckRollup") or []
    if not checks:
        print(f"{n}\tpending\t{title}\t")
        continue
    names = [c.get("name") or "" for c in checks]
    conclusions = [c.get("conclusion") or "" for c in checks]
    statuses = [c.get("status") or "" for c in checks]
    bad = [nm for nm, c in zip(names, conclusions) if c in BAD]
    if bad:
        print(f"{n}\tred\t{title}\t" + ",".join(bad))
    elif all(s == "COMPLETED" for s in statuses) and all(c == "SUCCESS" for c in conclusions):
        print(f"{n}\tgreen\t{title}\t")
    else:
        print(f"{n}\tpending\t{title}\t")
'

# _open_prs <repo-dir> -> one TSV line per currently open PR, or non-zero if gh could not be
# asked (a transient network error, not "no open PRs" — those two must never look alike).
_open_prs() {
    local repo_dir="$1" json rc
    json="$(cd "$repo_dir" && gh pr list --state open \
        --json number,title,headRefName,statusCheckRollup 2>/dev/null)"
    rc=$?
    [ "$rc" -eq 0 ] || return 1
    printf '%s' "$json" | python3 -c "$_PR_STATUS_PY"
}

# _pr_transitions <repo-dir> <repo-name> — the transition engine for one repo.
_pr_transitions() {
    local repo_dir="$1" repo_name="$2"
    local statefile snapshot rc
    statefile="$(_pr_state_file "$repo_name")"
    snapshot="$(_open_prs "$repo_dir")"; rc=$?
    [ "$rc" -eq 0 ] || return 0

    local -A prev_status=() prev_title=()
    local n st title
    if [ -r "$statefile" ]; then
        while IFS=$'\t' read -r n st title; do
            [ -n "$n" ] || continue
            prev_status["$n"]="$st"; prev_title["$n"]="$title"
        done < "$statefile"
    fi

    local -A cur_status=() cur_title=()
    local fail is_new
    while IFS=$'\t' read -r n st title fail; do
        [ -n "$n" ] || continue
        cur_status["$n"]="$st"; cur_title["$n"]="$title"
        is_new=0
        if [ -z "${prev_status[$n]+x}" ]; then
            is_new=1
            _report "OPENED #$n $title [$repo_name]"
        fi
        if [ "$is_new" -eq 1 ] || [ "$st" != "${prev_status[$n]:-}" ]; then
            case "$st" in
                red)
                    _report "FAIL RED #$n $title [$repo_name]${fail:+: $fail}"
                    ;;
                green)
                    _report "GREEN #$n $title [$repo_name]: lands automatically"
                    ;;
            esac
        fi
    done <<< "$snapshot"

    local m final
    for m in "${!prev_status[@]}"; do
        [ -n "${cur_status[$m]+x}" ] && continue
        final="$(cd "$repo_dir" && gh pr view "$m" --json state -q .state 2>/dev/null)"
        case "$final" in
            MERGED) _report "MERGED #$m ${prev_title[$m]} [$repo_name]" ;;
            CLOSED) _report "CLOSED #$m ${prev_title[$m]} [$repo_name]" ;;
            # Unresolved (gh unreachable, or the PR vanished from both calls): keep tracking
            # it rather than drop it silently — the next tick gets another chance to resolve
            # what became of it (law-a-control-that-cannot-check-must-refuse).
            *) cur_status["$m"]="${prev_status[$m]}"; cur_title["$m"]="${prev_title[$m]}" ;;
        esac
    done

    mkdir -p "$STATE_DIR"
    local tmp="$statefile.tmp"
    : > "$tmp"
    for m in "${!cur_status[@]}"; do
        printf '%s\t%s\t%s\n' "$m" "${cur_status[$m]}" "${cur_title[$m]}" >> "$tmp"
    done
    mv "$tmp" "$statefile"
}

# _branchless_prs <repo-dir> <repo-name> <base> — a spira/* branch ahead of base with no open
# PR yet. pr-mode only: a push-mode repo merges directly (no PR ever exists) and a queue-mode
# branch not yet batched is ordinary, not a stray.
_branchless_prs() {
    local repo_dir="$1" repo_name="$2" base="$3"
    git -C "$repo_dir" fetch --quiet 2>/dev/null || true
    local ref short ahead pr_n
    while IFS= read -r ref; do
        ref="${ref#  }"
        case "$ref" in *" -> "*) continue ;; esac
        short="${ref#origin/}"
        [ "$short" = "$ref" ] && continue
        case "$short" in spira/*) ;; *) continue ;; esac
        ahead="$(git -C "$repo_dir" rev-list --count "origin/$base..origin/$short" 2>/dev/null)"
        case "$ahead" in ""|0) continue ;; esac
        pr_n="$(cd "$repo_dir" && gh pr list --head "$short" --state open \
            --json number --jq 'length' 2>/dev/null)" || pr_n=""
        case "$pr_n" in ""|0) _emit "⚠ BRANCH $short: no PR [$repo_name]" ;; esac
    done < <(git -C "$repo_dir" branch -r 2>/dev/null)
}

# _pr_notify_tick — one pass over every repo in the repo-map, whatever its land mode.
_pr_notify_tick() {
    local name repo land base
    while IFS= read -r name; do
        [ -n "$name" ] || continue
        repo="$(repo_root "$name" 2>/dev/null)" || continue
        [ -d "$repo/.git" ] || continue
        land="$(repo_land "$name" 2>/dev/null)"
        _pr_transitions "$repo" "$name"
        if [ "$land" = pr ]; then
            base="$(repo_base "$name" 2>/dev/null)"; [ -n "$base" ] || base="main"
            _branchless_prs "$repo" "$name" "$base"
        fi
    done < <(repo_names 2>/dev/null)
}

_pr_notify_write_health() {
    local interval="$1"
    mkdir -p "$(dirname "$HEALTH_FILE")"
    printf 'ok %s %s\n' "$(date +%s)" "$interval" > "$HEALTH_FILE.tmp" \
        && mv "$HEALTH_FILE.tmp" "$HEALTH_FILE"
}

# cmd_health — the watchd health probe. Fails when the watch loop has never polled, or its
# last poll is older than three intervals (it is hung or dead).
cmd_health() {
    [ -r "$HEALTH_FILE" ] || {
        echo "pr-notify: never polled ($HEALTH_FILE absent)" >&2
        return 1
    }
    local st t iv now age
    read -r st t iv < "$HEALTH_FILE"
    case "$t" in ''|*[!0-9]*) t=0 ;; esac
    case "$iv" in ''|*[!0-9]*) iv=90 ;; esac
    now="$(date +%s)"
    age=$(( now - t ))
    if [ "$age" -gt $(( 3 * iv )) ]; then
        echo "pr-notify: last poll ${age}s ago (interval ${iv}s) — the watcher is hung or dead" >&2
        return 1
    fi
    return 0
}

# cmd_actionable [FILE] — the read-time filter for a pr-notify backlog. FILE defaults to this
# watcher's own log. A line survives only if its repo (the trailing `[name]`) is still in the
# repo-map — the log outlives a repo-map edit, so a name that was removed keeps its old lines
# standing forever unless something checks them against the map AS IT IS NOW, not as it was
# when the line was written — and only if SPIRA_ACTIONABLE still marks it worth a look.
# Repeats of the exact same line collapse to one: the transition engine reports a state CHANGE,
# so an identical line seen again means something upstream of it re-announced state that had
# not changed, and the reader should see that once, not once per poll.
cmd_actionable() {
    local file="${1:-$LOG_FILE}"
    [ -r "$file" ] || return 0
    if [ -z "${SPIRA_ACTIONABLE:-}" ]; then
        echo "pr-notify: SPIRA_ACTIONABLE is empty — that would match every line" >&2
        return 1
    fi
    local mapped; mapped="$(repo_names 2>/dev/null)"
    local line repo
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        # THE REPO TAG IS THE LAST BRACKETED, SPACE-FREE TOKEN — not necessarily the last
        # thing on the line: a RED line's failing-check names trail after it. A PR title is
        # free text and could coincidentally bracket something of its own, but never without
        # a space inside, which every repo-map name is required to be.
        repo="$(grep -oE '\[[^][[:space:]]+\]' <<< "$line" | tail -n1)"
        if [ -n "$repo" ]; then
            repo="${repo#[}"; repo="${repo%]}"
            grep -qxF "$repo" <<< "$mapped" || continue
        fi
        printf '%s\n' "$line" | grep -E -- "$SPIRA_ACTIONABLE" || continue
    done < "$file" | awk '!seen[$0]++'
}

# cmd_watch [--interval S] [--ticks N] — the watchd daemon body. Runs forever (or --ticks
# times, for a test), scanning every repo and writing a health record each pass.
cmd_watch() {
    local interval=90 ticks="" tick=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --interval) interval="${2:?--interval needs a value}"; shift 2 ;;
            --ticks)    ticks="${2:?--ticks needs a value}"; shift 2 ;;
            *) echo "pr-notify.sh: watch: unknown argument '$1'" >&2; return 2 ;;
        esac
    done
    while :; do
        _pr_notify_tick
        _pr_notify_write_health "$interval"
        tick=$((tick + 1))
        [ -n "$ticks" ] && [ "$tick" -ge "$ticks" ] && return 0
        sleep "$interval"
    done
}

# SOURCEABLE, AND SILENT WHEN IT IS. A T1 test wanting only _PR_STATUS_PY, _open_prs or
# _pr_transitions (pure functions once `gh` is stubbed) would otherwise trigger a live
# repo-map scan on source — the same seam watchd.sh and incident.sh already open.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    case "${1:-}" in
        --show|-s|"") _pr_notify_tick ;;
        watch)      shift; cmd_watch "$@" ;;
        health)     cmd_health; exit $? ;;
        actionable) shift; cmd_actionable "$@"; exit $? ;;
        *) echo "usage: pr-notify.sh [--show] | watch [--interval S] [--ticks N] | health | actionable [FILE]" >&2
           exit 2 ;;
    esac
fi
