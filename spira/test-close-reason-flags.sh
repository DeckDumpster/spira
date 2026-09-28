#!/usr/bin/env bash
#
# test-close-reason-flags.sh — close-reason-flags.py's classifiers, table-tested directly
#   against canned reason strings. No database, no cockpit.sh livelock.
#
#   ./test-close-reason-flags.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# detect_close_reason (raw, unmasked scan for a law-no-close-reason-admits-unfinished
# phrase — "PERMANENT FIX NEEDED", a temporary fix, "mitigated-only", "TODO") and
# check_unfiled_follow (a follow-on phrase — "builders should", "at scale", "the real
# fix", "follow-up" — with no tracking reference) are the exact functions
# detect_invalid_closed in lib.sh execs from this one file, so the detector and this
# suite cannot disagree. Every permutation of both belongs here, once, rather than
# paying a database reset per phrase (test-livelock.sh, before this suite existed, spent
# most of its 19 resets purely to exercise these two functions).
#
# check_close_reason (quote-masked, mention-file-aware) is the close-time FENCE's own
# function, used through aeon.sh and the close-reason-flags.py CLI; test-ops-closing.sh
# already table-tests it end to end (its own area — not duplicated here).
#
# test-livelock.sh keeps ONE row of each family proving detect_invalid_closed's output
# reaches cockpit.sh livelock's report — the integration, not the classification.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): a positive control proves
# the classifier can fire before any negative control is believed.
#
# tier: T1
# covers: spira/close-reason-flags.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# Load both functions the same way detect_invalid_closed does — exec, so a suite bug here
# cannot diverge from the shared contract's own loading path.
FLAGS_NS="$(python3 -c '
ns = {"__name__": ""}
exec(open("'"$HERE"'/close-reason-flags.py").read(), ns)
print("ok" if "detect_close_reason" in ns and "check_unfiled_follow" in ns else "missing")
')"
[ "$FLAGS_NS" = ok ] || bail "close-reason-flags.py did not define both functions on exec"

invalid_closed() {   # invalid_closed <reason> -> the matched phrase, or empty
    REASON="$1" python3 -c '
import os
ns = {"__name__": ""}
exec(open("'"$HERE"'/close-reason-flags.py").read(), ns)
hit = ns["detect_close_reason"](os.environ["REASON"])
print(hit or "")'
}

unfiled_follow() {   # unfiled_follow <reason> [id-prefix] -> the matched phrase, or empty
    REASON="$1" PREFIX="${2:-sp}" python3 -c '
import os
ns = {"__name__": ""}
exec(open("'"$HERE"'/close-reason-flags.py").read(), ns)
hit = ns["check_unfiled_follow"](os.environ["REASON"], os.environ["PREFIX"])
print(hit or "")'
}

echo "test-close-reason-flags.sh"

# ==========================================================================================
echo
echo "detect_close_reason — INVALID-CLOSED phrases:"
# ==========================================================================================
want "PERMANENT FIX NEEDED admits unfinished work" \
    "PERMANENT FIX NEEDED" "$(invalid_closed 'PERMANENT FIX NEEDED: add real detection')"
want "a temporary workaround is flagged" \
    "temporary workaround" "$(invalid_closed 'Applied a temporary workaround in the unit drop-in; landed.')"
want "mitigated-only is flagged" \
    "mitigated-only" "$(invalid_closed 'This is mitigated-only; the root cause remains.')"
want "a bare TODO is flagged" \
    "TODO" "$(invalid_closed 'TODO: revisit once the API stabilizes.')"
want "a quoted MENTION of a statute phrase still surfaces — no masking here" \
    "TEMPORARY WORKAROUND" \
    "$(invalid_closed 'pair added — bad-reason ("TEMPORARY WORKAROUND") is reopened; clean reason stays closed')"

echo
echo "detect_close_reason — negative controls:"
echo
is "a clean close reason is not flagged" \
    "" "$(invalid_closed 'fixed: sp-ll-clean commit abc123 landed on main')"
is "'temporary' describing something other than the fix is not flagged" \
    "" "$(invalid_closed 'Default derived from nproc and landed. The drop-in was a temporary measure and is removed.')"
is "'workaround' alone does not discriminate — it can be complete and landed" \
    "" "$(invalid_closed 'Workaround implemented by aeon-yojimbo, verified, landed on origin/main. Ready for merge.')"

# ==========================================================================================
echo
echo "check_unfiled_follow — UNFILED-FOLLOW phrases without a tracking reference:"
# ==========================================================================================
want "'builders should' with no bead id is flagged" \
    "builders should" "$(unfiled_follow 'Builders should add external_ref to bd list --json output.')"
want "'at scale' with no bead id is flagged" \
    "at scale" "$(unfiled_follow 'This does not hold up at scale and needs revisiting.')"
want "'the real fix' with no bead id is flagged" \
    "the real fix" "$(unfiled_follow 'Worked around for now; the real fix is deeper.')"
want "'follow-up' with no bead id is flagged" \
    "follow-up" "$(unfiled_follow 'Landed. A follow-up would harden the edge cases.')"

echo
echo "check_unfiled_follow — tracking-reference exemptions:"
echo
is "a bead id acquits the follow-on phrase" \
    "" "$(unfiled_follow 'Builders should add external_ref to bd list --json. Tracked as sp-80br6.')"
is "an owner/repo#N GitHub reference acquits it" \
    "" "$(unfiled_follow 'Builders should add external_ref to bd list --json. Filed as owner/repo#42.')"
is "an https:// URL acquits it" \
    "" "$(unfiled_follow 'The real fix is tracked at https://github.com/owner/repo/issues/42.')"
is "a non-default id prefix still acquits it" \
    "" "$(unfiled_follow 'Builders should fix the schema. Tracked as tt-abc1.' tt)"

echo
echo "check_unfiled_follow — other negative controls:"
echo
is "'upstream' alone does not trigger — it names a destination, not a remainder" \
    "" "$(unfiled_follow 'Moot upstream — the sentinel already handles this. Closed.')"
is "'workaround' alone does not trigger — it is not a follow-on phrase" \
    "" "$(unfiled_follow 'Workaround implemented, verified, landed. Ready for merge.')"
is "no follow-on phrase at all is not flagged" \
    "" "$(unfiled_follow 'fixed: sp-ll-clean commit abc123 landed on main')"

tl_summary
