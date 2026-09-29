#!/usr/bin/env bash
#
# groomer.sh — graph hygiene operations for the Spira DAG.
#
#   groomer.sh sweep          [--dry-run]                                     apply mechanical livelock remedies
#   groomer.sh split-piece    <original-id> [bd create args...]              file one piece of a split, on its own branch
#   groomer.sh supersede      <id> --with <successor>                        mark a bead superseded by another
#   groomer.sh close          <id> --evidence <text>                         close a bead whose premise is gone
#   groomer.sh correct-lane   <id> --lane <lane>                             correct a mislabelled lane label
#   groomer.sh depends-on-fix <bug-id> --fix <id> --evidence <text>         link bug to in-flight fix, order accordingly
#   groomer.sh unpoison       <id> --cause <c> --evidence <text>            credit a harness-caused attempt, lift spira-poison
#   groomer.sh triage-poison  <id> --verdict <work-fault|drop> --evidence <text>  close out a work-caused poison charge
#   groomer.sh unwanted       ...                                            REFUSED — exits 2 always
#
# WHAT IT DOES NOT DO:
#   It does NOT close a bead as unwanted. Unwanted changes the backlog's declared
#   desired state — a POLICY call — and it belongs to Ryan by the escalation policy.
#   This refusal is in this code, not in a sentence in the brief.
#
#   It does NOT re-prioritise. Priority management is Ryan's or the scheduler's.
#
# SPLIT AND MERGE:
#   Split and merge are compositional operations the aeon performs by calling
#   split-piece (for new pieces), supersede (for the original) and this
#   script. They do not have a single atomic subcommand because they are not
#   one operation — they are sequences, and each step records its own
#   evidence on the bead. split-piece exists, rather than a bare `bd create
#   --parent`, because that call inherits every label from the parent
#   (law-one-aeon-one-worktree: a bead's branch IS a label, so an unguarded
#   split hands every piece the PARENT's branch, and four children of one
#   bead resolved to one branch by construction).
#
# EXIT:
#   0  success
#   1  usage error / missing required argument
#   2  refused: the operation violates groomer policy
#
# covers: spira/groomer.sh spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=conf.sh
. "$HERE/conf.sh"

BD_CMD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:-.}"

usage() {
    printf 'usage: groomer.sh sweep|split-piece|supersede|close|correct-lane|depends-on-fix|unpoison|triage-poison|unwanted ...\n' >&2
    exit 1
}

cmd="${1:-}"; [ $# -gt 0 ] && shift
case "$cmd" in

  sweep)
    # groomer.sh sweep [--dry-run]
    # Runs detect_livelocked and applies three mechanical remedies before the model pass:
    #   ask-no-overseer → add overseer label (decisions pane remedy)
    #   ci-stuck               → strip awaiting-ci (repo is not pr-mode; no run will ever clear it)
    #   unmapped-repo, litter  → close (no description, no notes — predicate is fully computable)
    # unclaimable → REPORT only; partition repair is a judgment.
    #
    # Then runs the whole-graph STATE scan (sp-0qp7s) — no partition filter, unlike the
    # LIVELOCK worklist above, which is why it is a second pass rather than folded into the
    # same loop:
    #   landed-but-open        → close it; the base already carries the work (fully computable)
    #   closed-no-branch       → label spira-dropped; nothing was ever committed
    #   closed-never-landed    → reopen for rebase (conflict) or reopen batch-ready (clean);
    #                            either way the bead was claimed done and is not
    #   blocked-by-unlanded    → note only; the blocker's own reopen above is the remedy
    #
    # Then a scan over the incident partition alone (sp-18v9k, law-no-decision-work-has-a-
    # persona-owner): an incident bead whose recorded branch already carries a commit is
    # rerouted to the builders (SPIRA_INCIDENT_LABEL → SPIRA_PLAN_LABEL). Ops has no Edit or
    # Write tool, so that commit cannot be Ops's own work — fully computable, and it cannot
    # fire on a genuinely operational incident, which never produces one.
    _sw_dry=0
    while [ $# -gt 0 ]; do
      case "$1" in
        --dry-run) _sw_dry=1; shift ;;
        *) printf 'groomer: sweep: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done

    # shellcheck source=lib.sh
    . "$HERE/lib.sh"
    # shellcheck source=lc.sh
    . "$HERE/lc.sh"

    _sw_log() {
        local _ts _msg
        _ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        _msg="$_ts groom: sweep: $*"
        printf '%s\n' "$_msg"
        [ -d "${SPIRA_RUN:-}" ] && printf '%s\n' "$_msg" >> "$SPIRA_RUN/groom.log" 2>/dev/null || true
    }

    _sw_n=0
    _sw_ll="$(detect_livelocked 2>/dev/null)"
    if [ -z "$_sw_ll" ]; then
        _sw_log "no livelocked beads"
    fi

    while IFS= read -r _sw_line; do
        [ -n "$_sw_line" ] || continue
        case "$_sw_line" in
            LIVELOCK\ *\ ask-no-overseer\ *)
                _sw_rest="${_sw_line#LIVELOCK }"
                _sw_bid="${_sw_rest%% *}"
                _sw_log "OVERSEER $_sw_bid — adding overseer label; decisions pane cannot see this bead"
                if [ "$_sw_dry" -eq 0 ]; then
                    "$BD_CMD" -C "$DB" label add "$_sw_bid" overseer >/dev/null 2>&1 \
                        || printf 'groomer: sweep: WARN overseer label add failed for %s\n' "$_sw_bid" >&2
                fi
                _sw_n=$((_sw_n+1))
                ;;
            LIVELOCK\ *\ ci-stuck\ *)
                _sw_rest="${_sw_line#LIVELOCK }"
                _sw_bid="${_sw_rest%% *}"
                _sw_log "UNSTUCK $_sw_bid — stripping ${SPIRA_CI_LABEL}; repo not pr-mode, no run will ever clear this label"
                if [ "$_sw_dry" -eq 0 ]; then
                    "$BD_CMD" -C "$DB" label remove "$_sw_bid" "${SPIRA_CI_LABEL:?}" >/dev/null 2>&1 \
                        || printf 'groomer: sweep: WARN label remove %s failed for %s\n' "$SPIRA_CI_LABEL" "$_sw_bid" >&2
                    lc_unhold "$_sw_bid" wait groomer.sh || true
                fi
                _sw_n=$((_sw_n+1))
                ;;
            LIVELOCK\ *\ unmapped-repo\ *)
                _sw_rest="${_sw_line#LIVELOCK }"
                _sw_bid="${_sw_rest%% *}"
                _sw_reason="${_sw_rest#* — }"
                _sw_bj="$("$BD_CMD" -C "$DB" show "$_sw_bid" --json 2>/dev/null)"
                if [ -z "$_sw_bj" ]; then
                    _sw_log "REPORT $_sw_bid unmapped-repo — bd show failed; leaving for model. $_sw_reason"
                    continue
                fi
                _sw_pred="$(printf '%s\n' "$_sw_bj" | python3 "$HERE/groomer-litter-predicate.py" 2>/dev/null)"
                _sw_has_content="$(printf '%s\n' "$_sw_pred" | sed -n 's/^HAS_CONTENT //p')"
                _sw_meta="$(printf '%s\n' "$_sw_pred" | sed -n 's/^META //p')"
                if [ "${_sw_has_content:-0}" = "0" ]; then
                    _sw_log "CLOSED $_sw_bid — litter: no description, $_sw_meta. Detector: $_sw_reason"
                    if [ "$_sw_dry" -eq 0 ]; then
                        "$BD_CMD" -C "$DB" close "$_sw_bid" --reason-file - <<< \
                            "litter: no description, $_sw_meta. Detector: $_sw_reason"
                    fi
                    _sw_n=$((_sw_n+1))
                else
                    _sw_log "REPORT $_sw_bid unmapped-repo — has description; leaving for model. $_sw_reason"
                fi
                ;;
            LIVELOCK\ *\ unclaimable\ *)
                _sw_rest="${_sw_line#LIVELOCK }"
                _sw_bid="${_sw_rest%% *}"
                _sw_reason="${_sw_rest#* — }"
                _sw_log "REPORT $_sw_bid unclaimable — $_sw_reason"
                ;;
        esac
    done <<< "$_sw_ll"

    # ---- whole-graph STATE scan (sp-0qp7s): landed-but-open, closed-never-landed, false blockers ----
    _sw_reopened=""
    _sw_lbo="$(detect_landed_but_open 2>/dev/null)"
    if [ -n "$_sw_lbo" ]; then
        while IFS= read -r _sw_line; do
            [ -n "$_sw_line" ] || continue
            case "$_sw_line" in
                STATE\ *\ landed-but-open\ *)
                    _sw_rest="${_sw_line#STATE }"
                    _sw_bid="${_sw_rest%% *}"
                    _sw_reason="${_sw_rest#* landed-but-open — }"
                    _sw_log "CLOSED $_sw_bid — landed-but-open: $_sw_reason"
                    if [ "$_sw_dry" -eq 0 ]; then
                        "$BD_CMD" -C "$DB" close "$_sw_bid" --reason-file - <<< \
                            "landed-but-open: $_sw_reason"
                    fi
                    _sw_n=$((_sw_n+1))
                    ;;
            esac
        done <<< "$_sw_lbo"
    fi

    _sw_cul="$(detect_closed_unlanded_states 2>/dev/null)"
    if [ -n "$_sw_cul" ]; then
        while IFS= read -r _sw_line; do
            [ -n "$_sw_line" ] || continue
            case "$_sw_line" in
                STATE\ *\ closed-no-branch\ *)
                    _sw_rest="${_sw_line#STATE }"
                    _sw_bid="${_sw_rest%% *}"
                    _sw_reason="${_sw_rest#* closed-no-branch — }"
                    _sw_log "DROPPED $_sw_bid — closed-no-branch: $_sw_reason"
                    if [ "$_sw_dry" -eq 0 ]; then
                        "$BD_CMD" -C "$DB" label add "$_sw_bid" spira-dropped >/dev/null 2>&1
                        "$BD_CMD" -C "$DB" note "$_sw_bid" "Labeled spira-dropped by groomer.sh sweep: closed-no-branch — $_sw_reason" >/dev/null 2>&1
                    fi
                    _sw_n=$((_sw_n+1))
                    ;;
                STATE\ *\ closed-never-landed\ conflict\ *)
                    _sw_rest="${_sw_line#STATE }"
                    _sw_bid="${_sw_rest%% *}"
                    _sw_reason="${_sw_rest#*closed-never-landed conflict }"
                    _sw_log "REOPENED $_sw_bid — closed-never-landed, needs a rebase: $_sw_reason"
                    if [ "$_sw_dry" -eq 0 ]; then
                        bead_reopen "$_sw_bid" closed-never-landed-conflict \
                            "Reopened by groomer.sh sweep: closed but its branch does not merge cleanly into the base — $_sw_reason. Needs a rebase before it can land."
                    fi
                    _sw_reopened="$_sw_reopened $_sw_bid"
                    _sw_n=$((_sw_n+1))
                    ;;
                STATE\ *\ closed-never-landed\ batch-ready\ *)
                    _sw_rest="${_sw_line#STATE }"
                    _sw_bid="${_sw_rest%% *}"
                    _sw_reason="${_sw_rest#*closed-never-landed batch-ready }"
                    _sw_log "REOPENED $_sw_bid — closed-never-landed, batch-ready: $_sw_reason"
                    if [ "$_sw_dry" -eq 0 ]; then
                        bead_reopen "$_sw_bid" closed-never-landed-batch-ready \
                            "Reopened by groomer.sh sweep: closed but never landed — $_sw_reason. The branch merges cleanly; it is batch-ready to requeue as-is."
                    fi
                    _sw_reopened="$_sw_reopened $_sw_bid"
                    _sw_n=$((_sw_n+1))
                    ;;
            esac
        done <<< "$_sw_cul"
    fi

    if [ -n "$(printf '%s' "$_sw_reopened" | tr -d '[:space:]')" ]; then
        _sw_fb="$(detect_false_blockers "$_sw_reopened" 2>/dev/null)"
        if [ -n "$_sw_fb" ]; then
            while IFS= read -r _sw_line; do
                [ -n "$_sw_line" ] || continue
                case "$_sw_line" in
                    STATE\ *\ blocked-by-unlanded\ *)
                        _sw_rest="${_sw_line#STATE }"
                        _sw_bid="${_sw_rest%% *}"
                        _sw_reason="${_sw_rest#* blocked-by-unlanded }"
                        _sw_log "NOTED $_sw_bid — false blocker: $_sw_reason"
                        if [ "$_sw_dry" -eq 0 ]; then
                            "$BD_CMD" -C "$DB" note "$_sw_bid" "groomer.sh sweep: this bead was blocked-by-unlanded — $_sw_reason. The blocker was reopened this pass; this bead should become ready once it lands." >/dev/null 2>&1
                        fi
                        _sw_n=$((_sw_n+1))
                        ;;
                esac
            done <<< "$_sw_fb"
        fi
    fi

    # ---- incident partition: a bead whose branch already carries a commit is code, not
    # an operational incident — reroute it to the builders (sp-18v9k) ----
    _sw_inc="$(detect_incident_needs_builder 2>/dev/null)"
    if [ -n "$_sw_inc" ]; then
        while IFS= read -r _sw_line; do
            [ -n "$_sw_line" ] || continue
            case "$_sw_line" in
                STATE\ *\ incident-is-code\ *)
                    _sw_rest="${_sw_line#STATE }"
                    _sw_bid="${_sw_rest%% *}"
                    _sw_reason="${_sw_rest#*incident-is-code — }"
                    _sw_log "REROUTED $_sw_bid — incident-is-code: $_sw_reason"
                    if [ "$_sw_dry" -eq 0 ]; then
                        "$BD_CMD" -C "$DB" label remove "$_sw_bid" "${SPIRA_INCIDENT_LABEL:?}" >/dev/null 2>&1
                        "$BD_CMD" -C "$DB" label add "$_sw_bid" "${SPIRA_PLAN_LABEL:?}" >/dev/null 2>&1
                        "$BD_CMD" -C "$DB" note "$_sw_bid" "Moved by groomer.sh sweep: $SPIRA_INCIDENT_LABEL -> $SPIRA_PLAN_LABEL. $_sw_reason" >/dev/null 2>&1
                    fi
                    _sw_n=$((_sw_n+1))
                    ;;
            esac
        done <<< "$_sw_inc"
    fi

    _sw_log "sweep complete; acted on $_sw_n bead(s)"
    ;;

  split-piece)
    # groomer.sh split-piece <original-id> [bd create args...]
    #
    # `bd create --parent` inherits every label from the parent by default, and a bead's
    # branch affinity IS a label (`branch:<name>` — bead_branch, lib.sh): sp-zs04v was split
    # into four children this way and all four inherited branch:spira/sp-zs04v, resolving to
    # the PARENT's branch by construction and putting three live aeon sessions in one
    # worktree (law-one-aeon-one-worktree).
    #
    # bd set-state removes the previous branch: label atomically — it is a single-valued
    # dimension, the same guarantee correct-lane above relies on — so recording the new
    # piece's own branch right after creation is enough to undo whatever create inherited.
    # The id (and so the branch name) is not known until create returns it, which is why
    # this cannot be folded into a single bd call.
    #
    # delivers: is not single-valued, so nothing overwrites it the way set-state overwrites
    # branch: — it is stripped explicitly. It is an evidence claim scoped to ONE bead
    # (aeon.sh, sentinel.sh CHECK 5); a piece that inherits it would claim delivery of
    # evidence — a file path, a child-bead link — that was never its own to verify against.
    id="${1:-}"; [ $# -gt 0 ] && shift
    [ -z "$id" ] && { printf 'groomer: split-piece: original bead id required\n' >&2; exit 1; }
    new_id="$("$BD_CMD" -C "$DB" create --parent "$id" --silent "$@")" \
        || { printf 'groomer: split-piece: bd create failed\n' >&2; exit 1; }
    [ -n "$new_id" ] || { printf 'groomer: split-piece: bd create returned no id\n' >&2; exit 1; }
    "$BD_CMD" -C "$DB" set-state "$new_id" "branch=spira/$new_id" >/dev/null \
        || { printf 'groomer: split-piece: could not record branch on %s\n' "$new_id" >&2; exit 1; }
    _sp_inherited_delivers="$("$BD_CMD" -C "$DB" show "$new_id" --json 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    print(",".join(l for l in (d[0].get("labels") or []) if l.startswith("delivers:")))
except Exception: pass
' 2>/dev/null)"
    [ -n "$_sp_inherited_delivers" ] \
        && "$BD_CMD" -C "$DB" label remove "$new_id" "$_sp_inherited_delivers" >/dev/null 2>&1
    printf '%s\n' "$new_id"
    ;;

  supersede)
    # groomer.sh supersede <id> --with <successor>
    #
    # Records the supersedes edge that aeon.sh and CHECK 5 honour: a closed bead with a
    # supersedes edge is not reopened as "closed without landing" — its work landed under
    # the successor's id. The close reason alone is not read by those checks. Without this
    # edge, merging two duplicate beads produces a bead the sentinel reopens on every pass.
    id="${1:-}"; [ $# -gt 0 ] && shift
    successor=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --with)
          [ $# -lt 2 ] && { printf 'groomer: --with requires a value\n' >&2; exit 1; }
          successor="$2"; shift 2 ;;
        *) printf 'groomer: supersede: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done
    [ -z "$id" ]        && { printf 'groomer: supersede: bead id required\n' >&2; exit 1; }
    [ -z "$successor" ] && { printf 'groomer: supersede: --with <successor> required\n' >&2; exit 1; }
    "$BD_CMD" -C "$DB" supersede "$id" --with "$successor"
    ;;

  close)
    # groomer.sh close <id> --evidence <text>
    #
    # Closes a bead whose premise is gone — the thing it was filed to address no longer
    # exists or no longer applies. Evidence is REQUIRED: a close without evidence is
    # indistinguishable from an unwanted-close, which this script refuses. The evidence
    # is written as the close reason, so the next reader can verify it was legitimate.
    id="${1:-}"; [ $# -gt 0 ] && shift
    evidence=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --evidence)
          [ $# -lt 2 ] && { printf 'groomer: --evidence requires a value\n' >&2; exit 1; }
          evidence="$2"; shift 2 ;;
        *) printf 'groomer: close: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done
    [ -z "$id" ] && { printf 'groomer: close: bead id required\n' >&2; exit 1; }
    if [ -z "$evidence" ]; then
        printf 'groomer: close: --evidence <text> is required\n' >&2
        printf 'groomer: a close without evidence may be an unwanted-close in disguise;\n' >&2
        printf 'groomer: use the escalation path for that (law-escalate-decisions-not-problems)\n' >&2
        exit 1
    fi
    "$BD_CMD" -C "$DB" close "$id" --reason-file - <<< "$evidence"
    ;;

  correct-lane)
    # groomer.sh correct-lane <id> --lane <lane>
    #
    # Sets the lane dimension on a bead. bd set-state removes the previous lane: label
    # atomically, so exactly one lane: label remains after this call regardless of what
    # the bead carried before — single-valuedness is enforced by the substrate, not by
    # discipline.
    id="${1:-}"; [ $# -gt 0 ] && shift
    lane=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --lane)
          [ $# -lt 2 ] && { printf 'groomer: --lane requires a value\n' >&2; exit 1; }
          lane="$2"; shift 2 ;;
        *) printf 'groomer: correct-lane: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done
    [ -z "$id" ]   && { printf 'groomer: correct-lane: bead id required\n' >&2; exit 1; }
    [ -z "$lane" ] && { printf 'groomer: correct-lane: --lane <lane> required\n' >&2; exit 1; }
    "$BD_CMD" -C "$DB" set-state "$id" "lane=$lane"
    ;;

  depends-on-fix)
    # groomer.sh depends-on-fix <bug-id> --fix <bead-id> --evidence "<why this fix covers it>"
    #
    # Links a bug to its in-flight fix and creates a dependency edge. The bug leaves the
    # ready queue until the fix lands, then returns as the check on the fix. The association
    # is asserted by a human or agent that read both, never inferred.
    #
    # Validates:
    #   - fix bead exists and is not closed
    #   - --evidence is required (the reason is the point)
    # After linking and depending, the bug reappears in bd ready when the fix lands.
    bug_id="${1:-}"; [ $# -gt 0 ] && shift
    fix_id=""
    evidence=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --fix)
          [ $# -lt 2 ] && { printf 'groomer: --fix requires a value\n' >&2; exit 1; }
          fix_id="$2"; shift 2 ;;
        --evidence)
          [ $# -lt 2 ] && { printf 'groomer: --evidence requires a value\n' >&2; exit 1; }
          evidence="$2"; shift 2 ;;
        *) printf 'groomer: depends-on-fix: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done
    [ -z "$bug_id" ] && { printf 'groomer: depends-on-fix: bug id required\n' >&2; exit 1; }
    [ -z "$fix_id" ] && { printf 'groomer: depends-on-fix: --fix <bead-id> required\n' >&2; exit 1; }
    [ -z "$evidence" ] && { printf 'groomer: depends-on-fix: --evidence <text> is required\n' >&2; exit 1; }

    # Validate that the fix bead is not closed. Use quiet mode to suppress normal output,
    # capture the status field. bd show exits 1 if bead does not exist; we check for
    # CLOSED status specifically.
    fix_show_output="$("$BD_CMD" -C "$DB" show "$fix_id" --json 2>&1)"
    if [ $? -ne 0 ]; then
        printf 'groomer: depends-on-fix: fix bead %s does not exist\n' "$fix_id" >&2
        exit 1
    fi
    fix_status="$(printf '%s\n' "$fix_show_output" | grep -o '"status":"[^"]*"' | head -1 | cut -d'"' -f4)"
    if [ "$fix_status" = "CLOSED" ]; then
        printf 'groomer: depends-on-fix: fix bead %s is already closed — cannot depend on a closed bead\n' "$fix_id" >&2
        exit 1
    fi

    # Create the dependency: bug depends on fix.
    "$BD_CMD" -C "$DB" dep add "$bug_id" "$fix_id" || exit 1

    # Record the evidence on the bug.
    "$BD_CMD" -C "$DB" note "$bug_id" "Parked behind fix $fix_id: $evidence"
    ;;

  unpoison)
    # groomer.sh unpoison <id> --cause <c> --evidence <text>
    #
    # Lifts spira-poison when the charge was the HARNESS's fault, not the work's: a
    # pre-session death, a branch collision, yield-headless (the session backgrounded a test
    # batch and ended its turn), a gate that was still running when the release fired, or a
    # precondition that is now satisfied. --cause and --evidence are both REQUIRED — a lift
    # without evidence is indistinguishable from an ungrounded amnesty, which is exactly what
    # attempts.sh clear already provides for the operator's own judgement; this tool exists
    # so the groomer's judgement leaves the same kind of trace.
    #
    # Delegates to `spira-claim unpoison --credit <cause>` (spira-claim/DESIGN.md §8): the
    # unjudged credit, the poison.cleared floor, the lifecycle hold, the note and the ask,
    # verified by CHECK 4's own decision.
    id="${1:-}"; [ $# -gt 0 ] && shift
    cause=""
    evidence=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --cause)
          [ $# -lt 2 ] && { printf 'groomer: --cause requires a value\n' >&2; exit 1; }
          cause="$2"; shift 2 ;;
        --evidence)
          [ $# -lt 2 ] && { printf 'groomer: --evidence requires a value\n' >&2; exit 1; }
          evidence="$2"; shift 2 ;;
        *) printf 'groomer: unpoison: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done
    [ -z "$id" ] && { printf 'groomer: unpoison: bead id required\n' >&2; exit 1; }
    if [ -z "$cause" ]; then
        printf 'groomer: unpoison: --cause <c> is required (e.g. pre-session-death, branch-collision, yield-headless, gate-still-running, precondition-satisfied)\n' >&2
        exit 1
    fi
    if [ -z "$evidence" ]; then
        printf 'groomer: unpoison: --evidence <text> is required\n' >&2
        printf 'groomer: a lift without evidence may be an ungrounded amnesty in disguise;\n' >&2
        printf 'groomer: name the sessions and the harness fault this cause credits\n' >&2
        exit 1
    fi

    _up_out="$("${SPIRA_CLAIM_BIN:?SPIRA_CLAIM_BIN is unset — source conf.sh}" unpoison \
        --bead "$id" --cause "$cause: $evidence" --credit "$cause" --actor groomer)"; _up_rc=$?
    printf '%s\n' "$_up_out"
    case "$_up_out" in
        "OK   $id:"*) ;;
        *) printf 'groomer: unpoison: %s was not cleared (rc=%s) — see above\n' "$id" "$_up_rc" >&2; exit 1 ;;
    esac
    printf 'UNPOISONED %s cause=%s\n' "$id" "$cause"
    ;;

  triage-poison)
    # groomer.sh triage-poison <id> --verdict <work-fault|drop> --evidence <text>
    #
    # The other side of poison triage from `unpoison`: unpoison lifts a HARNESS-caused
    # charge; this closes out a WORK-caused one. A poisoned bead admits no claim (aeon.sh
    # refuses it at claim time), so a triage that leaves the label standing with a note
    # reading "poison stands, next claim must fix X" strands the bead forever — nothing
    # can ever be the next claim. "Poison stands" is a legal outcome only for `drop`,
    # which removes the bead from ready by closing it, not by leaving it stuck open.
    #
    #   work-fault   lifts spira-poison and the lifecycle poison hold, so the bead is
    #                claimable again — the fix this triage names IS the next claim. The
    #                attempt count is floored (bump_poison_cleared, same as unpoison) but
    #                NOT discounted: no requeued/unjudged event is written, because the
    #                charged attempts really were the work's fault and stay charged.
    #   drop         closes the bead and labels it spira-dropped: the triage decided this
    #                is not worth a next claim at all.
    id="${1:-}"; [ $# -gt 0 ] && shift
    verdict=""
    evidence=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --verdict)
          [ $# -lt 2 ] && { printf 'groomer: --verdict requires a value\n' >&2; exit 1; }
          verdict="$2"; shift 2 ;;
        --evidence)
          [ $# -lt 2 ] && { printf 'groomer: --evidence requires a value\n' >&2; exit 1; }
          evidence="$2"; shift 2 ;;
        *) printf 'groomer: triage-poison: unknown option: %s\n' "$1" >&2; exit 1 ;;
      esac
    done
    [ -z "$id" ] && { printf 'groomer: triage-poison: bead id required\n' >&2; exit 1; }
    case "$verdict" in
      work-fault|drop) ;;
      *) printf 'groomer: triage-poison: --verdict must be work-fault or drop (got %s)\n' "${verdict:-<empty>}" >&2; exit 1 ;;
    esac
    if [ -z "$evidence" ]; then
        printf 'groomer: triage-poison: --evidence <text> is required\n' >&2
        exit 1
    fi

    if [ "$verdict" = drop ]; then
        "$BD_CMD" -C "$DB" close "$id" --reason-file - <<< "GROOM: Poison triage — DROP. $evidence" || exit 1
        "$BD_CMD" -C "$DB" label add "$id" spira-dropped >/dev/null 2>&1
        printf 'DROPPED %s\n' "$id"
        exit 0
    fi

    # shellcheck source=lib.sh
    . "$HERE/lib.sh"
    # shellcheck source=lc.sh
    . "$HERE/lc.sh"

    _tp_labels="$("$BD_CMD" -C "$DB" label list "$id" 2>/dev/null)" || _tp_labels=""
    if ! grep -q spira-poison <<< "$_tp_labels"; then
        printf 'groomer: triage-poison: %s does not carry spira-poison — nothing to triage\n' "$id" >&2
        exit 1
    fi

    bump_poison_cleared "$id" "work-fault-triage"
    "$BD_CMD" -C "$DB" label remove "$id" spira-poison >/dev/null 2>&1 \
        || { printf 'groomer: triage-poison: could not remove spira-poison from %s\n' "$id" >&2; exit 1; }
    poison_asked_clear "$id"
    lc_unhold "$id" poison groomer.sh >/dev/null 2>&1 || true
    "$BD_CMD" -C "$DB" note "$id" "GROOM: Poison triage — WORK'S FAULT. $evidence Poison lifted (not credited — the charged attempts stand); the fix this triage names is the next claim. A poison.cleared event floors the attempt count so this does not immediately re-poison." >/dev/null 2>&1
    printf 'TRIAGED %s verdict=work-fault\n' "$id"
    ;;

  unwanted)
    # REFUSED. Closing a bead as unwanted changes the backlog's declared desired state —
    # a POLICY call, not a hygiene decision. That judgement belongs to Ryan by the
    # escalation policy (law-escalate-decisions-not-problems). This refusal is in the
    # code, not in a sentence in the brief.
    printf 'groomer: REFUSED — closing a bead as unwanted is a policy decision, not a hygiene operation.\n' >&2
    printf 'groomer: escalate to Ryan: "$SPIRA_HOME/mail.sh" send operator --from "<sender>" --subject "<question>" --kind question --default "close <id> as unwanted"\n' >&2
    exit 2
    ;;

  *)
    usage
    ;;
esac
