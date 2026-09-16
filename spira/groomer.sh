#!/usr/bin/env bash
#
# groomer.sh — graph hygiene operations for the Spira DAG.
#
#   groomer.sh supersede      <id> --with <successor>                        mark a bead superseded by another
#   groomer.sh close          <id> --evidence <text>                         close a bead whose premise is gone
#   groomer.sh correct-lane   <id> --lane <lane>                             correct a mislabelled lane label
#   groomer.sh depends-on-fix <bug-id> --fix <id> --evidence <text>         link bug to in-flight fix, order accordingly
#   groomer.sh unwanted       ...                                            REFUSED — exits 2 always
#
# WHAT IT DOES NOT DO:
#   It does NOT close a bead as unwanted. Unwanted is a product decision about
#   what the system should do, and it belongs to Ryan by the escalation policy.
#   This refusal is in this code, not in a sentence in the brief.
#
#   It does NOT re-prioritise. Priority management is Ryan's or the scheduler's.
#
# SPLIT AND MERGE:
#   Split and merge are compositional operations the aeon performs by calling bd
#   create (for new pieces), supersede (for the original) and this script. They
#   do not have their own subcommands because they are not atomic operations —
#   they are sequences, and each step records its own evidence on the bead.
#
# EXIT:
#   0  success
#   1  usage error / missing required argument
#   2  refused: the operation violates groomer policy
#
# covers: spira/groomer.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=conf.sh
. "$HERE/conf.sh"

BD_CMD="${SPIRA_BD:-bd}"
DB="${SPIRA_DB:-.}"

usage() {
    printf 'usage: groomer.sh supersede|close|correct-lane|depends-on-fix|unwanted ...\n' >&2
    exit 1
}

cmd="${1:-}"; [ $# -gt 0 ] && shift
case "$cmd" in

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

  unwanted)
    # REFUSED. Closing a bead as unwanted is a product decision about what the system
    # should do, not a hygiene decision. That judgement belongs to Ryan by the escalation
    # policy (law-escalate-decisions-not-problems). This refusal is in the code, not in
    # a sentence in the brief.
    printf 'groomer: REFUSED — closing a bead as unwanted is a product decision, not a hygiene operation.\n' >&2
    printf 'groomer: escalate to Ryan: "$SPIRA_HOME/mail.sh" send operator --from "<sender>" --subject "<question>" --kind question --default "close <id> as unwanted"\n' >&2
    exit 2
    ;;

  *)
    usage
    ;;
esac
