#!/usr/bin/env bash
#
# test-ops-closing.sh — an incident closed with no runbook behind it has its close undone.
#
#   ./test-ops-closing.sh
#
# THE DEFECT THIS REPRODUCES. The Ops brief has called the closing rule — *an incident
# resolved without an SOP must produce one* — "not optional" since the day it was written,
# and it was disobeyed six times in a single day. An instruction that nothing checks is a
# custom; the whole ladder here says the deliverable is a mechanism, so this is the check
# that makes silence cost something.
#
# WHAT "SILENCE" MEANS, EXACTLY, and why the distinction is the entire suite. A session may
# end three honest ways, and each is one command:
#
#   nothing on the shelf fit; something new was diagnosed   sop.sh write
#   an SOP fit but was incomplete                           sop.sh write   (the upsert)
#   an SOP fit and its CHECK confirmed                      sop.sh applied --check pass
#
# The third row is what keeps this from firing on the good case. "It matched, it held, it
# taught us nothing new" is the outcome a healthy shelf produces most of the time and is
# creditable; a check that poisoned it would punish the sessions the design wants. So every
# case here is a PAIR (law-absence-needs-a-positive-control): a silent session is required to
# poison, and each honest ending is required NOT to — including the two that are merely
# cheap. A check that never fires and a check that always fires look the same from a report
# that only ever contains one of them.
#
# THE FOUR EDGES WORTH NAMING, because each is a way the rule could go wrong in the direction
# nobody would notice:
#
#   --held no       satisfies. It is an honest outcome of a runbook that genuinely fitted,
#                   and demanding an amendment on top of it would make `--held yes` the
#                   cheapest exit — a lie, in the one field the shelf is measured by.
#   --check fail    does NOT satisfy. That is the session's own statement that nothing on the
#                   shelf applied, which is row one, and row one's exit is a write.
#   a retirement    does NOT satisfy. Removing a runbook is curation; it is not the thing the
#                   incident was supposed to leave behind.
#   an OLD record   does not satisfy a LATER session. The question is what this session
#                   recorded, not whether the bead has ever been recorded against.
#
# AND IT DECLINES TO JUDGE WHAT IT CANNOT READ. An unreadable ledger is not an absence, and
# treating one as an absence would poison every incident closed on the day the database is
# down — the day a runbook is worth most.
#
# THE PERSONA IS A FIXTURE NAMED SOMETHING ELSE, on purpose. The rule is declared by a fayth
# (FAYTH_SOP_REQUIRED) rather than keyed on the string "ops" in aeon.sh, so the suite pins a
# non-default: a healer that is not called ops is still held to the rule, and a builder is
# not. Asserting through the shipped ops.fayth would pass just as well against a check with
# the persona's name written into it.
#
# Driven through the REAL aeon.sh and the REAL sop.sh against a real bd on a throwaway
# fixture, with a shim standing in for the model. What is under test is a comparison of two
# database reads either side of a session, and a stub of either side would be a second
# implementation of the thing in question (law-prefer-the-real-dependency).
#
# defect: sp-9pyr
# covers: spira/aeon.sh spira/sop.sh spira/close-reason-flags.py spira/chamber/ops.fayth spira/chamber/ops.md spira/test-ops-closing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-ops-closing.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-ops-closing
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up opsclosing || { echo "test-ops-closing: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

HOMEDIR="$TMP/home"; mkdir -p "$HOMEDIR/chamber"
# SCAR: the fixture's cp list omitted suite-covers.sh after sp-dt8u added it to lib.sh.
# lib.sh sources suite-covers.sh at boot; without it a "No such file" error goes to stderr,
# which 2>&1 in the sop helper merges into stdout, inflating sop digest | grep -c . by 1.
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/sop.sh" "$HERE/suite-covers.sh" "$HERE/close-reason-flags.py" "$HOMEDIR/"
cp -r "$HERE/actors" "$HOMEDIR/" 2>/dev/null || true
RUN="$TMP/run"; mkdir -p "$RUN"
REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$REPO_MAP"

# A NON-DEFAULT LEDGER PATH. The shipped one sits under SPIRA_RUN; this one does not, so a
# program that derived the path rather than reading the key would write where this suite
# never looks — and a fixture's planted records can never land in a real count
# (law-gates-run-in-a-clean-environment).
LEDGER="$TMP/elsewhere/applications.jsonl"
LEDGER_OVERRIDE=""

# TWO FAYTHS, AND NEITHER IS CALLED ops. `healer` declares the rule; `builder` does not, and
# the pair is what proves the check is bound to the declaration rather than to a name.
for f in healer builder; do
    cat > "$HOMEDIR/chamber/$f.fayth" <<FAYTH
FAYTH_NAME=$f
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
    printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\nsop at {{SOP}}\n{{PARK}}\n' \
        > "$HOMEDIR/chamber/$f.md"
done
printf 'FAYTH_SOP_REQUIRED=1\n' >> "$HOMEDIR/chamber/healer.fayth"

# THE SHIM IS THE SESSION. It always commits and always closes, so the commit half of the
# verdict is satisfied in every case here and the only thing under test is what the session
# recorded. The guard is not decoration: conf.sh REPLACES $PATH, so a suite shimming `claude`
# by PATH alone would run the real model against a real account.
BIN="$TMP/bin"; mkdir -p "$BIN"
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-ops-closing: aeon.sh has no SPIRA_AGENT injection point — refusing to run the real model" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
# WHAT THIS SESSION DID ABOUT ITS RUNBOOK, chosen by the case under test. Everything runs
# through the real sop.sh at the path the brief itself was handed.
case "$(cat "$TMP/act")" in
    none) ;;
    write-new)
        "$SPIRA_HOME/sop.sh" write brand-new - >/dev/null 2>&1 <<'SOP'
SYMPTOM: something nobody had seen before
CHECK: systemctl is-failed fixture.service
FIX: restart it and watch the next run
SOP
        ;;
    amend)
        "$SPIRA_HOME/sop.sh" write disk-full - >/dev/null 2>&1 <<'SOP'
MATCH: (No space left on device|disk.*full)
SYMPTOM: a unit fails and the volume it writes to is full
CHECK: df -h /var | tail -1
FIX: clear the oldest artifacts, restart the unit, and confirm the next run is green
SOP
        ;;
    applied-yes)
        "$SPIRA_HOME/sop.sh" applied disk-full --bead "$id" --check pass --held yes >/dev/null 2>&1 ;;
    applied-no)
        "$SPIRA_HOME/sop.sh" applied disk-full --bead "$id" --check pass --held no >/dev/null 2>&1 ;;
    applied-fail)
        "$SPIRA_HOME/sop.sh" applied disk-full --bead "$id" --check fail --held unknown >/dev/null 2>&1 ;;
    retire)
        "$SPIRA_HOME/sop.sh" retire disk-full >/dev/null 2>&1 ;;
esac
case "$(cat "$TMP/act")" in
    bad-reason) bd -C "$SPIRA_DB" close "$id" --reason "DIAGNOSED: X. TEMPORARY WORKAROUND: Y must be removed once fix lands." >/dev/null 2>&1 ;;
    bad-reason-admit) bd -C "$SPIRA_DB" close "$id" --reason "TEMPORARY WORKAROUND: x is set until y lands" >/dev/null 2>&1 ;;
    bad-reason-mention) bd -C "$SPIRA_DB" close "$id" --reason 'pair added — bad-reason ("TEMPORARY WORKAROUND") is reopened; clean reason stays closed' >/dev/null 2>&1 ;;
    *)          bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1 ;;
esac
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

# THE ENVIRONMENT IS NAMED, NOT INHERITED. Two keys make this mandatory rather than tidy: an
# inherited SPIRA_CONF would let a real box decide these verdicts, and an inherited
# SPIRA_WIKI would send `sop.sh write`'s synthesis into a real wiki page — this suite writes
# SOPs, so that is not hypothetical. HOME is the real one because `bd` and `dolt` read their
# credentials from it, and SPIRA_PATH is passed because conf.sh rebuilds PATH from it.
run_aeon() {             # run_aeon <fayth> <act>
    printf '%s' "$2" > "$TMP/act"
    rm -rf "$RUN/worktree"
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" TMP="$TMP" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_WIKI="" \
        SPIRA_HOME="$HOMEDIR" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO_MAP="$REPO_MAP" SPIRA_AGENT="$BIN/claude" \
        SPIRA_SOP_LEDGER="${LEDGER_OVERRIDE:-$LEDGER}" \
        SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-}" \
        BEADS_NO_AUTO_IMPORT=1 \
        timeout 300 bash "$HOMEDIR/aeon.sh" "$1" > "$TMP/out" 2>&1
}
sop() {                  # the same program the aeon runs, in the same environment
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_WIKI="" \
        SPIRA_HOME="$HOMEDIR" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_SOP_LEDGER="${LEDGER_OVERRIDE:-$LEDGER}" BEADS_NO_AUTO_IMPORT=1 \
        timeout 120 bash "$HOMEDIR/sop.sh" "$@" 2>&1
}
seed() {                 # seed <id>
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"unit failed","status":"open","issue_type":"bug","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$_lbl" | testdb_seed
}
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
labels() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }
# THE NOTES AS THEY WERE WRITTEN. `bd show` wraps prose to a width, so an assertion against
# the rendered form passes or fails on where the wrap fell rather than on what was recorded.
notes()  { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | tr -s '[:space:]' ' '; }

# ONE RUNBOOK ON THE SHELF, rewritten after every reset because a hard reset takes the
# memories with it. Without it the `applied` cases have no slug to name and would fail for a
# reason that has nothing to do with the check.
shelf() {
    sop write disk-full - >/dev/null 2>&1 <<'SOP'
MATCH: (No space left on device|disk.*full)
SYMPTOM: a unit fails and the volume it writes to is full
CHECK: df -h /var | tail -1
FIX: clear the oldest artifacts, then restart the unit
SOP
}
fresh() {                # fresh <bead-id> — an empty world with one incident and one runbook
    testdb_reset
    rm -rf "$TMP/elsewhere"
    shelf
    seed "$1"
}

echo
echo "a session that recorded NOTHING — the close is undone and the bead is poisoned:"
fresh sp-oc-1; run_aeon healer none
is   "the bead is open again"              open  "$(field sp-oc-1 status)"
is   "and its claim is released"           ""    "$(field sp-oc-1 assignee)"
want "it is poisoned"                      "spira-poison" "$(labels sp-oc-1)"
want "the note says no runbook came out of it" "no runbook came out of it" "$(notes sp-oc-1)"
want "and names both ways it could have discharged the rule" "sop.sh applied --check pass" "$(notes sp-oc-1)"
want "and the log names the rule it broke" "REOPENED and POISONED" "$(cat "$TMP/out")"
# THE ATTEMPT LEDGER MUST NOT ALSO CHARGE THIS SESSION. The bead is open because the harness
# reopened it, and the teardown reads the session's trace, which is of a session that
# committed, closed and ran to its own end. Left to itself it files this as a worker that
# "did not survive to judge this bead" — about a session that judged it fine.
nowant "the session is not recorded as a worker that died" "did not survive" "$(notes sp-oc-1)"
want   "it is recorded as the harness's own requeue"       "sop-silent"      "$(notes sp-oc-1)"
nowant "and no reclaim rung was hung on it"                "sp-reclaim"      "$(labels sp-oc-1)"

echo
echo "a session that WROTE a new runbook — untouched, which is the first honest ending:"
fresh sp-oc-2; run_aeon healer write-new
is     "the bead stays closed"          closed "$(field sp-oc-2 status)"
nowant "it is not poisoned"             "spira-poison" "$(labels sp-oc-2)"
want   "and the verdict saw the write"  "closing-rule wrote=yes" "$(cat "$TMP/out")"
is     "the runbook is really on the shelf" "0" "$(sop show brand-new >/dev/null 2>&1; echo $?)"

echo
echo "a session that AMENDED the runbook that fitted — untouched, the second honest ending:"
fresh sp-oc-3; run_aeon healer amend
is     "the bead stays closed"      closed "$(field sp-oc-3 status)"
nowant "it is not poisoned"         "spira-poison" "$(labels sp-oc-3)"
# THE ASSERTION THE DIGEST EXISTS FOR. An amendment leaves the shelf exactly the size it was,
# so a check that counted SOPs would have poisoned this session.
want   "the amendment was seen even though the shelf did not grow" "closing-rule wrote=yes" "$(cat "$TMP/out")"
is     "and the shelf is still one runbook" "1" "$(sop digest | grep -c .)"

echo
echo "a session whose runbook FIT AND HELD — untouched. This is the case that must not fire:"
fresh sp-oc-4; run_aeon healer applied-yes
is     "the bead stays closed"                 closed "$(field sp-oc-4 status)"
nowant "it is not poisoned"                    "spira-poison" "$(labels sp-oc-4)"
want   "the verdict credits the application"   "applied=0" "$(cat "$TMP/out")"
nowant "and nothing was reopened"              "REOPENED" "$(cat "$TMP/out")"

echo
echo "a runbook that fit and did NOT hold — still untouched, because the truth must stay cheapest:"
fresh sp-oc-5; run_aeon healer applied-no
is     "the bead stays closed"  closed "$(field sp-oc-5 status)"
nowant "it is not poisoned"     "spira-poison" "$(labels sp-oc-5)"

echo
echo "a runbook whose CHECK did NOT confirm — poisoned, because that says nothing on the shelf fit:"
fresh sp-oc-6; run_aeon healer applied-fail
is   "the bead is open again"                 open "$(field sp-oc-6 status)"
want "and poisoned"                           "spira-poison" "$(labels sp-oc-6)"
# THE POSITIVE CONTROL FOR THE FILTER ITSELF: the record exists and is readable, so the
# poison above is the --check filter working and not the ledger having gone missing.
is   "the record it made is really in the ledger" "0" "$(sop log --bead sp-oc-6 >/dev/null 2>&1; echo $?)"

echo
echo "a session that only RETIRED a runbook — poisoned; curation is not what the incident owed:"
fresh sp-oc-7; run_aeon healer retire
is   "the bead is open again" open "$(field sp-oc-7 status)"
want "and poisoned"           "spira-poison" "$(labels sp-oc-7)"
is   "the shelf really did shrink"  "0" "$(sop digest | grep -c .)"

echo
echo "a BUILDER that closed without touching an SOP — normal work, and nothing happens to it:"
fresh sp-oc-8; run_aeon builder none
is     "the bead stays closed"     closed "$(field sp-oc-8 status)"
nowant "it is not poisoned"        "spira-poison" "$(labels sp-oc-8)"
nowant "and the check did not run at all" "closing-rule" "$(cat "$TMP/out")"

echo
echo "an OLDER session's record does not excuse a later silent one:"
fresh sp-oc-9
mkdir -p "$(dirname "$LEDGER")"
# A record for this very bead, made an hour ago by a session that is not this one. Written
# by hand because that is precisely what it is: prior history, not something this run did.
printf '{"ts":"2026-09-08T00:00:00Z","epoch":%s,"sop":"sop-disk-full","bead":"sp-oc-9","check":"pass","held":"yes","actor":"aeon-earlier","shelf":"ok","note":"ok","why":""}\n' \
    "$(( $(date -u +%s) - 3600 ))" >> "$LEDGER"
run_aeon healer none
is   "the ledger does hold a passing record for it" "0" "$(sop log --bead sp-oc-9 --check pass >/dev/null 2>&1; echo $?)"
is   "but this session recorded nothing, so it is reopened" open "$(field sp-oc-9 status)"
want "and poisoned"                                 "spira-poison" "$(labels sp-oc-9)"

echo
echo "an unreadable ledger is NOT an absence — nothing is poisoned on the day the harness is broken:"
fresh sp-oc-10
LEDGER_OVERRIDE="$TMP/corrupt.jsonl"; printf 'this is not json\nnor is this\n' > "$LEDGER_OVERRIDE"
run_aeon healer none
is     "the bead stays closed"                closed "$(field sp-oc-10 status)"
nowant "and is not poisoned"                  "spira-poison" "$(labels sp-oc-10)"
want   "the harness says it declined to judge" "closing rule NOT judged" "$(cat "$TMP/out")"
unset LEDGER_OVERRIDE

# THE POSITIVE CONTROL FOR THAT DECLINE. The same silent session against a readable ledger
# poisons, so the pass above is the corruption being detected and not the check having
# quietly stopped running.
fresh sp-oc-11; run_aeon healer none
is   "the same silence against a readable ledger still poisons" open "$(field sp-oc-11 status)"
want "and is poisoned"                                          "spira-poison" "$(labels sp-oc-11)"

echo
echo "the mechanism is declared, and Ops deliberately does not opt into it:"
# THE RULE IS DECLARED, NOT NAMED IN aeon.sh. Every case above ran as `healer`, which proves
# the check is not keyed on the string "ops"; this is the other half — that aeon.sh reads the
# key rather than the persona's name. Everything above still tests the MECHANISM, and the
# mechanism still works for any fayth that sets FAYTH_SOP_REQUIRED=1.
#
# OPS NO LONGER SETS IT, and that is the assertion this line now pins (per the operator,
# 2026-09-18). The rule made SILENCE the one outcome a session could not choose: close an
# incident without producing a runbook and the close was UNDONE, the bead reopened and
# labelled spira-poison. On the smallest model, under an eight minute wall, holding an
# unrestricted shell, "I looked and there is nothing to fix" became the most expensive
# available conclusion — so it was not concluded. An Ops aeon under exactly that pressure
# wrote a cleanup that classified every branch in the repository as garbage and deleted all
# of them, destroying 24 beads of unlanded work (sp-q27cp).
#
# ASSERTED AS =0 RATHER THAN DELETED. A removed assertion says nothing, and the next reader
# who finds a persona that plainly ought to produce runbooks would set it back to 1 without
# ever learning what that cost. This pins the choice and names the scar.
want "ops.fayth declares the rule OFF" "FAYTH_SOP_REQUIRED=0" "$(cat "$HERE/chamber/ops.fayth")"
want "aeon.sh binds the check to that key"   "FAYTH_SOP_REQUIRED"   "$(cat "$HERE/aeon.sh")"
nowant "and not to the persona's name"       "FAYTH\" = \"ops"      "$(cat "$HERE/aeon.sh")"

echo
echo "a close reason with a statute phrase is refused — this is the close-reason fence:"
# THE FENCE IS UNIVERSAL. It applies to every aeon regardless of FAYTH_SOP_REQUIRED.
# Using `builder` here (the persona that opted OUT of the SOP rule) proves the fence is
# bound to the close reason text, not to the persona's contract.
# POSITIVE CONTROL FIRST: a clean reason keeps the bead closed.
fresh sp-oc-12; run_aeon builder none
is   "a clean reason stays closed"              closed "$(field sp-oc-12 status)"
nowant "and the fence did not fire"             "REOPENED" "$(cat "$TMP/out")"

# THE FENCE: a statute phrase triggers a reopen.
fresh sp-oc-13; run_aeon builder bad-reason
is   "a statute phrase reopens the bead"        open   "$(field sp-oc-13 status)"
want "the note names the matched phrase"        "TEMPORARY WORKAROUND" "$(notes sp-oc-13)"
want "the log names the override"              "SPIRA_CLOSE_REASON_OVERRIDE" "$(cat "$TMP/out")"

echo
echo "the fence fires on a plain ADMIT but not on a quoted MENTION:"
# POSITIVE CONTROL: a reason that admits the phrase directly is refused.
fresh sp-oc-14; run_aeon builder bad-reason-admit
is   "an admit reopens the bead"                open   "$(field sp-oc-14 status)"
want "the note names the matched phrase"        "TEMPORARY WORKAROUND" "$(notes sp-oc-14)"

# NEGATIVE CONTROL: the same phrase inside a parenthetical is a mention, not an admission.
fresh sp-oc-15; run_aeon builder bad-reason-mention
is   "a quoted mention stays closed"            closed "$(field sp-oc-15 status)"
nowant "and the fence did not fire"             "REOPENED" "$(cat "$TMP/out")"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
