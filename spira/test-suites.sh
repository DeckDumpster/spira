#!/usr/bin/env bash
#
# test-suites.sh — every suite in the tree is run by something, and a red reaches a bead.
#
#   ./test-suites.sh
#
# WHAT THIS GUARDS. A suite that nothing runs is not a cheap test; it is a false record of
# coverage, and worse than no suite at all, because its existence is what stops anybody
# writing the check it was meant to be. Measured once: nine suites in the tree, four named by
# the landing gate, FIVE run by nothing, three of them landed the same night with their beads
# closed citing them as verification. The runner under test is the answer to that, and its own
# failure mode is identical in shape — it can quietly run a subset, file nothing, or record a
# green it never obtained, and every one of those looks exactly like a healthy pass.
#
# SO EVERY ABSENCE HERE IS PROVED THROUGH A PRESENCE (law-absence-needs-a-positive-control):
# before believing that a gated suite is not run, an ungated one is proved to be run by the
# same pass; before believing that a repeated red files no second bead, a CHANGED red is
# proved to file one; before believing a missing record means "never ran", a record is read
# off a suite that did.
#
# THE PLANTED SUITES ARE THE POINT. Nothing here names a real suite, because the property
# under test is that the runner needs no list: a suite is run BY EXISTING, so the fixture
# creates files and expects them to be picked up, and deletes one and expects it to stop. A
# fixture naming the tree's real suites would be asserting against today's tree.
#
# THE INTAKE IS THE REAL ONE, on a throwaway database (law-prefer-the-real-dependency). The
# dedupe under test is incident.sh's dedupe on the external ref, and a stub would reproduce
# whichever half of it the author remembered — which is how a partial model of `bd` twice made
# correct callers look broken here.
#
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT and the program runs under `env -i`
# (law-gates-run-in-a-clean-environment). Asserting against the shipped budget or the shipped
# priority passes just as well with the number written back into the code, which is the thing
# the key exists to stop.
#
# defect: sp-ewnb
# covers: spira/suites.sh spira/gate-suites spira/gate-spira.sh spira/incident.sh
# timeout: 180
# host-reason: testdb provides isolation; no host systemd or network contact required.
# scar: the landing-gate section's hand-written stub loop listed `exclude.sh inventory.sh
# hermetic.sh sop.sh` and omitted `literal-lint.sh` when c515322 added it as gate-spira.sh's
# fourth fence — gate refused with "literal-lint.sh is missing" on every gate invocation in the
# section; gate_fence_stubs derives the fence list from gate-spira.sh at call time (sp-vnxz).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/gate-fences.sh"
testdb_require test-suites
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suites || { echo "test-suites: could not build a fixture database"; exit 1; }

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$HERE/suite-covers.sh" "$SH/"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

# The knobs, every one of them away from the shipped default.
BUDGET=120; PERSUITE=20; STALE=3600; PRIO=3; REPONAME=fixture-repo

# THE FIXTURE NEEDS A REAL `bd`, AND IT IS REACHED THROUGH SPIRA_PATH RATHER THAN PATH.
# conf.sh REPLACES PATH outright, so a directory that is only on the caller's PATH has gone by
# the time the intake runs — and the intake reads "cannot reach the database" as "no open
# incident exists", which is the reading that files nothing and then reports a clean pass.
#
# THE BORROWED HOME IS CONFINED TO THAT ONE COMMAND. `bd` finds its own installation and
# dolt's configuration under the invoking user's HOME, so it cannot run under the scratch one
# the program under test is given — but the program under test must still not see a real HOME,
# or it would resolve a real database and a real client settings file. So the wrapper below is
# the seam: it hands `bd` the environment `bd` needs and nothing else changes hands, and what
# is borrowed from the box is one line long and visible rather than ambient.
#
# `type -P`, not `command -v`: the hermeticity fence judges `command -v bd` as a bare `bd`,
# which is the right call on every other line in this tree and wrong only on this one.
TOOLPATH="$TMP/bin"; mkdir -p "$TOOLPATH"
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$TOOLPATH/bd"
chmod +x "$TOOLPATH/bd"

# sut <subcommand> [VAR=value ...] — the runner in an environment holding nothing else.
#
# SPIRA_SUITES_RUNNER_VARS="" disables _suite_env inside suites.sh. Without it, sut
# passes SPIRA_HOME="$SH" explicitly, suites.sh sees SPIRA_HOME as set, and builds
# _suite_env="-u SPIRA_HOME" — wrapping every fixture suite in an extra `env` binary.
# That extra process in the setsid chain adds exec overhead and causes intermittent
# TIMEOUTs under load. Declaring no runner vars here prevents the wrapping; fixture
# suites run as plain `setsid bash`, which is the stable form the prior suite tests
# verified and which the confirming-run path (no RUNNER_VARS to strip) also uses.
#
# SPIRA_SUITES_SKIP_TESTDB=1 prevents suites.sh from building a shared embedded
# fixture on each call. The embedded availability check runs bd init in a temp dir
# (~6s) and testdb_up runs a second bd init (~6s) — ~12s per sut call. With ~18
# sut calls in this suite that cost adds ~216s, pushing the 180s budget past its
# limit. Fixture suites planted here do not use testdb, so no suite borrows it.
sut() {
    local cmd="$1"; shift
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_NOTIFY="$SH/ask.sh" FX_GATED_RAN="$TMP/gated.ran" \
        SPIRA_PATH="$TOOLPATH" SPIRA_SUITES_RUNNER_VARS="" SPIRA_INCIDENT_LOCK_WAIT="60" \
        SPIRA_SUITES_SKIP_TESTDB=1 \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
# plant <name> — the suite's body on stdin. No list is edited anywhere; existing is the whole
# of how a suite joins the run.
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }
clear_results() { find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true; }

B() { bd -C "$SPIRA_DB" "$@"; }
# beads <title-substring> -> how many OPEN beads carry it in their title.
#
# MATCHED ON THE TITLE, not on the external ref, because whether `external_ref` survives the
# JSON round trip is a fact about a version of bd rather than about the data — the intake's
# own dedupe already reads it through the server-side filter, which is the reading that
# matters. A count here that used a field bd might not emit would report "no bead was filed"
# for every case, including the ones that must file.
beads() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
key = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if key in (i.get("title") or ""): print(i["id"])
' "$1"
}
count() { printf '%s\n' "$1" | grep -c . || true; }
bead_field() {
    B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d[0] if isinstance(d, list) else d
print(d.get(sys.argv[1], ""))
' "$2"
}

# ======================================================================================
echo
echo "the shipped tree — every suite in it is claimed by the gate or by the timed run:"
# ======================================================================================
# THIS CASE IS ABOUT THE REAL TREE, deliberately, and it is the only one here that is. The
# complement is sound by construction, so what can actually go wrong is the gate's list naming
# a file that is not there — and gate-spira.sh fails closed on that, which refuses every
# branch until somebody notices. Cheap here, expensive to discover from a queue that stopped.
missing=""
while IFS= read -r line || [ -n "$line" ]; do
    line="${line%%#*}"; line="${line#"${line%%[![:space:]]*}"}"; line="${line%"${line##*[![:space:]]}"}"
    [ -n "$line" ] || continue
    [ -r "$HERE/../$line" ] || missing="$missing $line"
done < "$HERE/gate-suites"
is "gate-suites names only suites that exist" "" "$missing"

real="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_RUN="$RUN" SPIRA_SUITES_STATE="$TMP/none" bash "$HERE/suites.sh" list 2>&1)"
unclaimed=0
for f in "$HERE"/test-*.sh; do
    n="$(basename "$f")"
    case "$real" in
        *"$n"*gate*|*"$n"*timed*) ;;
        *) unclaimed=$(( unclaimed + 1 )); bad "$n is claimed by the gate or by the timed run" "neither" ;;
    esac
done
[ "$unclaimed" = 0 ] && ok "every spira/test-*.sh in this tree is claimed by one of the two"
# And the population really is the glob: this suite exists, so it must be in that listing.
want "the listing is the glob, not a list" "test-suites.sh" "$real"

# ======================================================================================
echo
echo "discovery is a glob — a planted suite is run because it exists:"
# ======================================================================================
printf '# the gated one, and nothing else\nspira/test-fx-gated.sh\n' > "$GATEF"

plant test-fx-green.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  ok    green"
S
plant test-fx-red.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  FAIL  the planted red: wanted [a] got [b]"
exit 1
S
plant test-fx-bare.sh <<'S'
#!/usr/bin/env bash
echo "  ok    no coverage declaration on this one"
S
plant test-fx-skip.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "SKIP no fixture server on this box"
exit 77
S
plant test-fx-gated.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
touch "$FX_GATED_RAN"
S

out="$(sut run)"; rc_reds=$?
want "a planted suite is run with no list edited"      "test-fx-green.sh" "$out"
want "a planted red is reported red"                   "RED" "$out"
[ -e "$TMP/gated.ran" ] \
    && bad "a gated suite is not run a second time by the timed pass" "test-fx-gated.sh ran" \
    || ok "a gated suite is not run a second time by the timed pass"
want "and the pass says which it skipped and why" "the landing gate already runs these" "$out"
want "naming it"                                  "test-fx-gated.sh" "$out"

# THE OMISSION IS REPORTED AND THE SUITE IS STILL RUN. Skipping an undeclared suite would
# rebuild the defect this program exists to end, inside the program that ends it.
want "a suite with no covers declaration is named" "test-fx-bare.sh" "$out"
want "as an omission, not as a skip"               "no \`# covers:\` declaration" "$out"
is   "and it has a result, so it really ran"       "ok" \
     "$( { read -r s1 _ < "$STATE/test-fx-bare.sh.result"; printf '%s' "${s1:-}"; } 2>/dev/null )"

# A run with reds exits 2 — suites ran and incidents were filed (routine), not 1
# (which is reserved for critical errors that abort before any suite runs).
is "a run with reds exits 2 (routine red, not a critical error)" "2" "$rc_reds"

# ======================================================================================
echo
echo "green is recorded, not silent — and a suite that never ran has no record:"
# ======================================================================================
# The positive control for every "absent" reading below: a record exists for a suite that ran,
# so a missing one means "never ran" rather than "the runner does not write these".
# Pre-declare so a missing file produces a clear FAIL rather than crashing with `st: unbound
# variable` — the original failure mode that filed sp-xoxxo.
st="" at="" secs=""
read -r st at secs _ < "$STATE/test-fx-green.sh.result" 2>/dev/null || true
is   "a green suite leaves a record"                "ok" "$st"
case "$at" in
    ''|*[!0-9]*) bad "with a timestamp, and it is now" "[$at]" ;;
    *) [ "$(( $(date +%s) - at ))" -lt 600 ] && ok "with a timestamp, and it is now" \
       || bad "with a timestamp, and it is now" "$(( $(date +%s) - at ))s old" ;;
esac
case "$secs" in ''|*[!0-9]*) bad "and how long it took" "[$secs]" ;; *) ok "and how long it took" ;; esac
[ -e "$STATE/test-fx-never.sh.result" ] \
    && bad "a suite that never ran has no record" "there is one" \
    || ok "a suite that never ran has no record"

is   "a 77 is its own status, neither pass nor failure" "skip" \
     "$( { read -r s2 _ < "$STATE/test-fx-skip.sh.result"; printf '%s' "${s2:-}"; } 2>/dev/null )"
want "and the pass says so"                            "SKIPPED" "$out"

# ======================================================================================
echo
echo "a red reaches a bead, in the builder's partition, carrying its output:"
# ======================================================================================
ids="$(beads 'test-fx-red.sh')"
is "one bead was filed for the red" "1" "$(count "$ids")"
id="$(printf '%s\n' "$ids" | head -1)"
if [ -n "$id" ]; then
    shown="$(B show "$id" 2>&1)"
    want "it names the suite"                           "test-fx-red.sh" "$shown"
    want "it carries the suite's own output"            "the planted red" "$shown"
    want "and says the fix is not blocked"              "nothing is blocked" "$shown"
    labels="$(B label list "$id" 2>&1)"
    want "it is labelled for the builders, not for Ops" "plan" "$labels"
    want "and names the repository from configuration"  "repo:$REPONAME" "$labels"
    nowant "and is not filed as an Ops incident"        "incident" "$labels"
    is "at the configured priority"                     "$PRIO" "$(bead_field "$id" priority)"
fi
is "and no bead was filed for the green one" "0" "$(count "$(beads 'test-fx-green.sh')")"
is "nor for the skip"                        "0" "$(count "$(beads 'test-fx-skip.sh')")"

# ======================================================================================
echo
echo "a persistent red files once — and a red that CHANGES files again:"
# ======================================================================================
sut run >/dev/null
is   "the same failure a second cycle is still one bead" "1" "$(count "$(beads 'test-fx-red.sh')")"
# sp-recur-N labels are no longer written (sp-lzt); recurrences are recorded as events and
# logged to the incident log as "... recurred (N) — <id>". Checking the log for the bead id
# in a "recurred" line is the correct signal that the dedup path ran and recorded the recurrence.
want "recorded as a recurrence on it"                    "$id" \
     "$(grep ' recurred ' "$RUN/incident.log" 2>/dev/null || true)"

# THE POSITIVE CONTROL FOR THAT SILENCE. A dedupe that swallowed everything would pass the
# case above just as well, so the same suite is made to fail DIFFERENTLY and must be heard.
plant test-fx-red.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  FAIL  an entirely different assertion: wanted [q] got [z]"
exit 1
S
sut run >/dev/null
is "a failure that changes is new information and files again" "2" \
   "$(count "$(beads 'test-fx-red.sh')")"

# ======================================================================================
echo
echo "timestamps in FAIL output do not fork the fingerprint (sp-zq0bw):"
# ======================================================================================
# THE DEFECT THIS PINS. ISO-8601 timestamps contain two-digit fields (month, day, hour,
# minute, second) that survive the [0-9]{3,} normaliser unchanged, so two identical failures
# separated by one clock tick produced different checksums and filed separate beads rather
# than bumping recurrence.
plant test-fx-timestamped.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  FAIL  a run still in progress says nothing about sp-pk-live, 65 passed / 1 failed"
echo "  detail: [$(date -u '+%Y-%m-%dT%H:%M:%SZ') spira: state: goal=running]"
exit 1
S
sut run >/dev/null
ts_id="$(beads 'test-fx-timestamped.sh' | head -1)"
is "first run of a timestamped failure files one bead" "1" "$(count "$(beads 'test-fx-timestamped.sh')")"
sleep 2  # ensure the wall-clock moves so the timestamp in the output changes
sut run >/dev/null
is "a second run at a later timestamp is still one bead" "1" "$(count "$(beads 'test-fx-timestamped.sh')")"
# sp-recur-N labels are no longer written (sp-lzt); check the incident log for the bead id
# in a "recurred" line — the dedup path logs "... recurred (N) — <id>" on each recurrence.
want "recorded as a recurrence on the same bead" "${ts_id:-none}" \
     "$(grep ' recurred ' "$RUN/incident.log" 2>/dev/null || true)"

# NEGATIVE CONTROL. A genuinely different failure must still file a fresh bead even when the
# only FAIL line it shares with the first is the timestamp token — the normaliser must not
# widen to the point that every timestamped failure looks the same.
plant test-fx-timestamped.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  FAIL  a completely different assertion: goal=stopped instead of goal=running"
echo "  detail: [$(date -u '+%Y-%m-%dT%H:%M:%SZ') spira: state: goal=stopped]"
exit 1
S
sut run >/dev/null
is "a genuinely different timestamped failure files a new bead" "2" \
   "$(count "$(beads 'test-fx-timestamped.sh')")"

rm -f "$SH/test-fx-timestamped.sh"

# ======================================================================================
echo
echo "a suite declares the priority of what it covers:"
# ======================================================================================
plant test-fx-urgent.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
# priority: 0
echo "  FAIL  urgent thing broke"
exit 1
S
sut run >/dev/null
uid="$(beads 'test-fx-urgent.sh' | head -1)"
is "a priority line beside the covers line is honoured" "0" "$(bead_field "${uid:-none}" priority)"

# ======================================================================================
echo
echo "a deleted suite stops being run, with no edit anywhere:"
# ======================================================================================
rm -f "$SH/test-fx-urgent.sh" "$SH/test-fx-red.sh"
out5="$(sut run)"; rc5=$?
nowant "a deleted suite is not run"   "test-fx-urgent.sh" "$out5"
want   "while the survivors still are" "test-fx-green.sh" "$out5"
is     "and with no red left the pass is green" "0" "$rc5"

# ======================================================================================
echo
echo "the budget is a wall, and the cursor is what makes stopping at it sound:"
# ======================================================================================
plant test-fx-slow.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
sleep 30
S
clear_results
BUDGET=2
out6="$(sut run)"
BUDGET=120
want "a pass out of budget says what it did not reach" "not reached this pass" "$out6"
# WITHOUT THE CURSOR the same suites would be run every pass and the last ones never — the
# defect this program exists to end, rebuilt inside it. So the cursor must name a suite that
# was left, and it must be one of this tree's rather than a stale name.
cur="$(cat "$STATE/cursor" 2>/dev/null)"
if [ -n "$cur" ] && [ -f "$SH/$cur" ]; then
    ok "the next pass is told where to start ($cur)"
    want "and that suite is named as unreached" "$cur" "$out6"
else
    bad "the next pass is told where to start" "cursor is [${cur:-empty}]"
fi
rm -f "$SH/test-fx-slow.sh" "$STATE/cursor"

# ======================================================================================
echo
echo "SPIRA_SUITES_MAXSEC caps the budget regardless of SPIRA_SUITES_BUDGET (sp-o060):"
# ======================================================================================
# THE PROBLEM THIS GUARDS. An operator-set SPIRA_SUITES_BUDGET can exceed the unit's
# TimeoutStartSec, causing every pass to be SIGTERMed. suites.sh must derive its effective
# budget from the smaller of the two. The unit injects SPIRA_SUITES_MAXSEC for exactly
# this: a config value cannot outrun the deadline systemd will enforce.
#
# POSITIVE CONTROL FIRST. The pass always prints "<N> suite(s)... <BUDGET>s budget" in its
# first line; without this check, a cap that only silences output would pass the next cases.
clear_results
out_nocap="$(sut run)"
want "positive control: the configured budget is reported" "${BUDGET}s budget" "$out_nocap"

# MAXSEC BELOW BUDGET. With MAXSEC=80, cap = 80 - 60 = 20. Budget was BUDGET=120, now 20.
# The reported budget line must reflect the cap, not the configured value.
clear_results
out_capped="$(sut run SPIRA_SUITES_MAXSEC=80)"
want "MAXSEC below BUDGET: capped budget is reported" "20s budget" "$out_capped"
nowant "and the original budget is not used" "${BUDGET}s budget" "$out_capped"

# MAXSEC ABOVE BUDGET. With BUDGET=2 and MAXSEC=9999 (cap=9939), the cap does not apply
# and the budget stays at 2. The pass may leave suites unreached because of the short budget.
clear_results
BUDGET=2
out_high="$(sut run SPIRA_SUITES_MAXSEC=9999)"
BUDGET=120
want "MAXSEC above BUDGET does not raise the effective budget" "2s budget" "$out_high"

# ======================================================================================
echo
echo "a suite whose last runtime exceeds the remaining budget is skipped, not timed out:"
# ======================================================================================
# POSITIVE CONTROL FIRST. A suite with no prior result is not pre-skipped — it falls
# through to the normal timeout path. Without this, a check that skipped everything
# would pass the budget-skip case just as well.
plant test-fx-budgetcheck.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
echo "  ok    budgetcheck suite ran (positive-control pass)"
S
rm -f "$STATE/test-fx-budgetcheck.sh.result"
BUDGET=40
sut run >/dev/null
BUDGET=120
# A suite that ran writes a result file; one that was pre-skipped does not. The main
# pass output does not print the suite's stdout on success, so the result file is the
# signal that the suite actually executed.
[ -e "$STATE/test-fx-budgetcheck.sh.result" ] \
    && ok "a suite with no prior result is not pre-skipped — result file was written" \
    || bad "a suite with no prior result is not pre-skipped" "no result file written"

# Now seed a result claiming 60s. The guard fires when last_secs(60) > 30
# and left(~40) < last_secs(60): suite is added to unreached, never started.
printf 'ok %s 60 -\n' "$(date +%s)" > "$STATE/test-fx-budgetcheck.sh.result"
BUDGET=40
out_bc="$(sut run)"
BUDGET=120
want   "a suite whose prior runtime exceeds the remaining budget is not reached" \
       "not reached" "$out_bc"
want   "and the suite appears in the unreached list" \
       "test-fx-budgetcheck.sh" "$out_bc"
nowant "and it does not appear as TIMEOUT"   "TIMEOUT"          "$out_bc"
nowant "and the suite body did not execute"  "budgetcheck suite ran" "$out_bc"
rm -f "$SH/test-fx-budgetcheck.sh" "$STATE/test-fx-budgetcheck.sh.result"

# ======================================================================================
echo
echo "an unreadable gate list is refused, never guessed:"
# ======================================================================================
# Reading it as "the gate runs nothing" would put every gated suite into the timed pass and
# double its cost while reporting that it had found more work to do. Nothing may run.
clear_results
mv "$GATEF" "$TMP/gate-suites.away"
out7="$(sut run)"; rc7=$?
is   "the pass refuses"  "1" "$rc7"
want "and says why"      "refusing to guess" "$out7"
is   "and ran nothing"   "0" "$(find "$STATE" -maxdepth 1 -name '*.result' | wc -l)"
want "and status reports the gated set as unknown" "?" "$(sut status)"
mv "$TMP/gate-suites.away" "$GATEF"

# ======================================================================================
echo
echo "status distinguishes 'nothing has run' from 'everything passed':"
# ======================================================================================
clear_results
st="$(sut status)"
want "with no results the oldest is \`?\` and not a zero" "?" "$st"
want "and the never-run count is the whole timed set"     "with no result yet" "$st"
sut run >/dev/null
st="$(sut status)"
nowant "after a pass the oldest result is a real age" "(nothing has run)" "$st"
want   "and the gated set is counted separately"      "gated" "$st"

# A RESULT PAST THE STALE BOUND IS NOT EVIDENCE. A stale pass and a runner that has stopped
# are the same silence from outside, so the bound is asserted rather than assumed — with the
# bound pinned to a non-default, so a literal in the code could not pass this.
#
# THE VALUE IS EXTRACTED, NOT MATCHED AS A SUBSTRING OF THE RENDERED LINE. The label is padded
# to a fixed width, so a hand-counted "label plus spaces plus 0" string never matches whatever
# the code prints and the assertion passes on every input — an assertion that cannot fail,
# which is the same all-clear-by-construction this whole program exists to end. So: the field
# under its own label, and the count is READ.
field() { printf '%s\n' "$2" | sed -n "s/^  $1  *//p" | head -1; }
is "with the bound long, no result is stale" "0" "$(field "timed results older than 1h" "$st")"
# THE POSITIVE CONTROL for the skip count: test-fx-skip.sh exits 77 and its status must be
# reported distinctly from the green suites in cmd_status.
skip_n="$(field "timed suites skipped at last run" "$st")"
case "$skip_n" in
    ''|0) bad "skip count is distinct from green in status" "the field reads [$skip_n]" ;;
    *)    ok  "skip count is distinct from green in status ($skip_n)" ;;
esac
STALE=1
sleep 2
st="$(sut status)"
STALE=3600
stale_n="$(field "timed results older than 0h" "$st")"
case "$stale_n" in
    ''|0) bad "a result past the stale bound stops counting as fresh" "the field reads [$stale_n]" ;;
    *)    ok "a result past the stale bound stops counting as fresh ($stale_n)" ;;
esac

# ======================================================================================
echo
echo "the landing gate reads the same file, and fails closed on a bad one:"
# ======================================================================================
# THE REAL gate-spira.sh, in a scratch tree of its own. That list decides whether anything
# lands at all, so the two ways it can be wrong — naming a suite that is not there, and naming
# none — are exercised against the program rather than asserted from its source.
GT="$TMP/gtree"
mkdir -p "$GT/spira"
git init -q -b main "$GT"
cp "$HERE/gate-spira.sh" "$HERE/suite-covers.sh" "$GT/spira/"
gate_fence_stubs "$GT/spira/gate-spira.sh" "$GT/spira"
# sop.sh is checked by gate-spira.sh outside the fence loop, so gate_fence_stubs does not
# create it; stub it separately so the gate reaches the suites section.
printf '#!/usr/bin/env bash\nexit 0\n' > "$GT/spira/sop.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$GT/spira/test-gt-ok.sh"
git -C "$GT" add -A
git -C "$GT" -c user.email=t@t -c user.name=t commit -q -m base

gate() {
    # SPIRA_SUITE_TIMEOUT=5 keeps the watchdog short so the test stays responsive.
    env -i PATH="$PATH" HOME="$TMP/home" SPIRA_SUITE_TIMEOUT=5 bash "$GT/spira/gate-spira.sh" 2>&1
}

printf 'spira/test-gt-ok.sh\n' > "$GT/spira/gate-suites"
gout="$(gate)"; grc=$?
is   "a gate whose list names a real suite passes" "0" "$grc"
want "and says that suite ran"                     "test-gt-ok.sh" "$gout"

printf '# nothing but a comment\n' > "$GT/spira/gate-suites"
gout="$(gate)"; grc=$?
is   "a list naming no suite is refused, never an empty pass" "1" "$grc"
want "and says so"                                 "names no suite" "$gout"

printf 'spira/test-gt-gone.sh\n' > "$GT/spira/gate-suites"
gout="$(gate)"; grc=$?
is   "a list naming a suite that is not there is refused" "1" "$grc"
want "before any suite has been paid for"                 "not here" "$gout"

rm -f "$GT/spira/gate-suites"
gout="$(gate)"; grc=$?
is   "and a missing list is refused too" "1" "$grc"

# ======================================================================================
echo
echo "a suite that leaks a background child does not wedge the pass (sp-04bd, sp-8x36):"
# ======================================================================================
# THE DEFECT THIS PINS. cmd_run used out="$(timeout ... bash $s 2>&1)". Command substitution
# reads until EOF ON THE PIPE, not until the child exits, so a suite that backgrounds anything
# inheriting stdout keeps the write end open after the suite itself has finished — and the
# runner blocks on a suite that already SUCCEEDED. timeout kills the script and never touches
# an orphaned grandchild, so the wall does not rescue it either.
#
# Measured before the fix: test-aeon-heartbeat.sh passes 22/0 standalone, yet the hourly pass
# recorded no result for it and none for the three suites alphabetically after it. Killing
# exactly the two orphaned `sleep` pids released a wedged reader within 2 seconds.
#
# 16031d8 fixed it by redirecting to a file and reading the file. It landed WITHOUT a test,
# which is why this exists: the failure is silent, it looks exactly like a slow suite, and the
# next leak would reproduce it with nothing to say so.
#
# THE SECOND SUITE IS THE ASSERTION THAT MATTERS. A leaker that merely takes its own timeout
# costs one result; a leaker that wedges the RUNNER costs every suite after it, which is the
# shape actually observed. So plant a name that sorts after the leaker and require its record.
clear_results
plant test-fx-leaky.sh <<'L'
#!/usr/bin/env bash
# Leaks a child holding stdout open far beyond the suite's own life.
sleep 300 &
echo "leaky ran"
exit 0
L
plant test-fx-zafter.sh <<'L'
#!/usr/bin/env bash
echo "the suite after the leaker ran"
exit 0
L

leak_out="$(sut run SPIRA_SUITES_BUDGET=60 SPIRA_SUITE_TIMEOUT=10)"; leak_rc=$?

is "the pass returns rather than hanging on the leaked child (rc=2: suite filed as red)" "2" "$leak_rc"
is "the leaker is marked red for leaving a background job" "red" \
   "$( { read -r ls _ < "$STATE/test-fx-leaky.sh.result"; printf '%s' "${ls:-MISSING}"; } 2>/dev/null )"
is "and so does the suite after it" "ok" \
   "$( { read -r zs _ < "$STATE/test-fx-zafter.sh.result"; printf '%s' "${zs:-MISSING}"; } 2>/dev/null )"
want "the pass names the suite after the leaker" "test-fx-zafter.sh" "$leak_out"

# Do not leave the fixture's own leaked child running for the rest of the suite.
pkill -P $$ -x sleep 2>/dev/null; true
rm -f "$SH/test-fx-leaky.sh" "$SH/test-fx-zafter.sh"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
