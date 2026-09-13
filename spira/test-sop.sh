#!/usr/bin/env bash
#
# test-sop.sh — an SOP application leaves a record, and a session that left none is
#               DISTINGUISHABLE from one that did.
#
#   ./test-sop.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The Ops brief told an aeon to match a runbook, run its CHECK, then run its FIX — and
# nothing wrote down that any of it happened. A session that matched an SOP and ignored it
# left exactly the trace of one that executed it faithfully, so nobody could say of any SOP
# on the shelf how often it fired, how often it was applied, or how often the incident came
# back anyway. `sop.sh applied` is that record and this is the suite that holds it.
#
# THE PROPERTY UNDER TEST IS A DISTINCTION, NOT A VALUE. Everything downstream — the check
# that fires on a silent Ops session, the numbers that will say which runbooks work — rests
# on being able to tell three states apart:
#
#   the ledger was read and holds a record for this bead          exit 0, the line on stdout
#   the ledger was read and holds nothing for this bead           exit 1, a TRUE absence
#   the ledger could not be read at all                           exit 2, NOT an absence
#
# A two-valued answer would merge the second and third, and the merged one reads as
# all-clear (law-absence-needs-a-positive-control). So every assertion below that something
# is ABSENT is preceded, in the same fixture, by proof that the same read finds the same
# thing when it is present — and the unreadable cases are asserted as 2 rather than as 1.
#
# A REAL `bd` ON A THROWAWAY DATABASE. Half of what `applied` must do is put a note on a
# bead, and a stub `bd` would reproduce the surface this suite remembers rather than the one
# `sop.sh` actually calls (law-prefer-the-real-dependency). The shelf is real too: the slug
# check reads `bd memories`, and its whole subtlety is telling "the shelf is empty" from "the
# shelf could not be read", which no model of bd would get right by accident.
#
# THE ENVIRONMENT IS EXPLICIT AND MINIMAL, and SPIRA_SOP_LEDGER and SOP_WHY_CAP ARE PINNED TO
# NON-DEFAULTS. A suite that inherits a real spira.conf asserts against one box, one that
# inherits SPIRA_WIKI writes into a real wiki page, and one that asserts against the shipped
# ledger path passes just as well if the code has that path written in — which is the thing
# the key exists to stop (law-gates-run-in-a-clean-environment).
#
# defect: sp-9p1a
# covers: spira/sop.sh spira/chamber/ops.md spira/chamber/*
# scar: nothing recorded that an SOP was matched and applied; a session that ignored a runbook left the same trace as one that executed it faithfully.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-sop.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sop
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sop || { echo "test-sop: could not build a fixture database"; exit 1; }

RUN="$TMP/run"; mkdir -p "$RUN"
# NON-DEFAULTS, BOTH OF THEM. The shipped ledger sits at $SPIRA_RUN/sop/applied.jsonl and the
# shipped why-cap is 400; asserting against either would pass with the value written into the
# code. This path is not under SPIRA_RUN at all, so a program that derived it rather than
# reading the key would write somewhere this suite never looks.
LEDGER="$TMP/elsewhere/sop-applications.jsonl"
LEDGER_OVERRIDE=""
WHY_CAP=30

# WHAT IS PASSED, AND WHY EACH ONE. `env -i` names the whole environment rather than
# inheriting it, so no real spira.conf decides a verdict here and no inherited SPIRA_WIKI
# sends `synth` into a real wiki page. Three things have to be passed anyway:
#
#   HOME        the REAL one. `bd` and `dolt` read their own configuration and credentials
#               from it, so a fixture home makes every database call fail — as "the shelf is
#               unreadable", which is a state this suite also tests for and would then be
#               asserting about the fixture rather than about the code.
#   PATH        where the binaries are, before conf.sh rewrites it.
#   SPIRA_PATH  and that rewrite is why. conf.sh REPLACES PATH with its own list so a systemd
#               timer resolves `bd`, and SPIRA_PATH is the key that puts the operator's
#               binaries back on the front of it. Omit it and the program under test loses
#               `bd` entirely, which presents as every database assertion failing at once.
sop() {                  # sop <args...> — the program under test, in a clean environment
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_RUN="$RUN" \
        SPIRA_DB="${SPIRA_DB_OVERRIDE:-$SPIRA_DB}" \
        SPIRA_SOP_LEDGER="${LEDGER_OVERRIDE:-$LEDGER}" SOP_WHY_CAP="$WHY_CAP" \
        BEADS_ACTOR="aeon-testops" BEADS_NO_AUTO_IMPORT=1 \
        timeout 120 bash "$HERE/sop.sh" "$@" 2>&1
}
sop_rc() {               # the same, but the caller wants the status and not the output
    sop "$@" >/dev/null 2>&1; printf '%s' "$?"
}
bdt() { bd -C "$SPIRA_DB" "$@"; }
# THE NOTES AS THEY WERE WRITTEN. `bd show` wraps prose to a width, so an assertion against
# the rendered form passes or fails on where the wrap fell rather than on what was recorded.
notes() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null; }

# TWO INCIDENTS AND ONE RUNBOOK. sp-t1 is the session that applies the SOP; sp-t2 is the
# session that matched it and recorded nothing. They are the same shape in every other
# respect, which is what makes the comparison mean something.
testdb_seed <<'JSONL'
{"id":"sp-t1","title":"unit failed: fixture-one","status":"open","issue_type":"bug","priority":1}
{"id":"sp-t2","title":"unit failed: fixture-two","status":"open","issue_type":"bug","priority":1}
JSONL

sop write disk-full - <<'SOP' >/dev/null 2>&1
MATCH: (No space left on device|disk.*full)
SYMPTOM: a unit fails and the volume it writes to is full
CHECK: df -h /var | tail -1
FIX: clear the oldest artifacts, then restart the unit
SOP

echo
echo "--- the shelf, and the ledger before anything is recorded"

out="$(sop list)"
want "the fixture SOP is on the shelf" "sop-disk-full" "$out"

# THE POSITIVE CONTROL FOR THE READ COMES FIRST, in its weakest form: with no ledger at all,
# `log` must say UNREADABLE (2) and not "no records" (1). If those were the same answer, every
# assertion after this one would be satisfied by a program that never wrote anything.
is "no ledger at all reads as unreadable, not as empty" "2" "$(sop_rc log --bead sp-t1)"

echo
echo "--- a recorded application"

out="$(sop applied disk-full --bead sp-t1 --check pass --held yes)"
is  "recording exits 0"                         "0" "$(sop_rc applied disk-full --bead sp-t1 --check pass --held unknown)"
want "it says what it recorded"                 "recorded sop-disk-full on sp-t1" "$out"
want "--held yes reads as a complete outcome"   "taught us nothing new" "$out"

want "the ledger is at the CONFIGURED path"     "sop-disk-full" "$(cat "$LEDGER" 2>/dev/null)"
is   "nothing was written to the default path"  "0" "$(ls "$RUN/sop" 2>/dev/null | wc -l)"

# EVERY FIELD DOWNSTREAM WILL READ, named here. A renamed field is the specific way this
# measurement stops working while continuing to look healthy.
line="$(head -1 "$LEDGER")"
fields="$(python3 -c '
import sys, json
r = json.loads(sys.argv[1])
print(" ".join("%s=%s" % (k, r.get(k)) for k in
      ("sop","bead","check","held","actor","shelf","note")))
print("ts=%s" % r["ts"]); print("epoch_is_int=%s" % isinstance(r["epoch"], int))
' "$line" 2>&1)"
want "the line names its SOP"      "sop=sop-disk-full" "$fields"
want "the line names its bead"     "bead=sp-t1"        "$fields"
want "the line records the CHECK"  "check=pass"        "$fields"
want "the line records held"       "held=yes"          "$fields"
want "the line records the actor"  "actor=aeon-testops" "$fields"
want "the shelf was verified"      "shelf=ok"          "$fields"
want "the note is recorded as landed" "note=ok"        "$fields"
want "the timestamp is ISO-8601 UTC" "Z"               "$fields"
want "the epoch is a number"       "epoch_is_int=True" "$fields"

echo
echo "--- THE DISTINCTION: a session that recorded nothing is not a session that recorded"

is "a bead with a record reads 0"          "0" "$(sop_rc log --bead sp-t1)"
is "a bead with NO record reads 1"         "1" "$(sop_rc log --bead sp-t2)"
want "and the one with a record prints it" "sp-t1" "$(sop log --bead sp-t1)"
nowant "the unrecorded bead prints nothing" "sp-t2" "$(sop log --bead sp-t1)"
is "filtering by SOP finds it"             "0" "$(sop_rc log --sop disk-full)"
is "filtering by an SOP nobody applied reads 1" "1" "$(sop_rc log --sop never-fired)"

echo
echo "--- the bead note, which is the half a human reads"

note="$(notes sp-t1)"
want "the note is on the bead"            "SOP sop-disk-full applied" "$note"
want "the note carries the CHECK result"  "CHECK pass"                "$note"
want "the note states the outcome in words" "taught us nothing new"   "$note"
# The positive control for the read above: the SAME query against the bead that recorded
# nothing must come back without it, or `want` would be passing on the string appearing
# anywhere at all.
nowant "and nothing was written onto the other bead" "SOP sop-disk-full applied" "$(notes sp-t2)"

echo
echo "--- append-only, and sorted by construction"

before="$(cat "$LEDGER")"; n_before="$(wc -l < "$LEDGER")"
sop applied disk-full --bead sp-t2 --check fail --held unknown >/dev/null 2>&1
is  "a second record adds a line"      "$((n_before + 1))" "$(wc -l < "$LEDGER")"
want "and leaves the first untouched"  "$before" "$(cat "$LEDGER")"
is  "the earlier line is still line 1" "$line"   "$(head -1 "$LEDGER")"
is  "the file is in epoch order"       "sorted"  "$(python3 -c '
import sys, json
e = [json.loads(l)["epoch"] for l in open(sys.argv[1]) if l.strip()]
print("sorted" if e == sorted(e) else "OUT OF ORDER: %s" % e)' "$LEDGER")"
is  "the bead that had no record now has one" "0" "$(sop_rc log --bead sp-t2)"

echo
echo "--- parseable without tooling"

is "every line is a JSON object" "ok" "$(python3 -c '
import sys, json
for i, l in enumerate(open(sys.argv[1]), 1):
    if not l.strip(): continue
    try:
        if not isinstance(json.loads(l), dict): print("line %d is not an object" % i); sys.exit()
    except Exception as e: print("line %d: %s" % (i, e)); sys.exit()
print("ok")' "$LEDGER")"
want "grep alone answers who applied what" "sop-disk-full" "$(grep sp-t2 "$LEDGER")"

echo
echo "--- the refusals, each one a typo that would poison the count"

is "an unknown slug is refused"            "1" "$(sop_rc applied no-such-sop --bead sp-t1 --check pass --held yes)"
want "and it says so"  "no such SOP" "$(sop applied no-such-sop --bead sp-t1 --check pass --held yes)"
is "a missing --bead AND --pass is refused" "1" "$(sop_rc applied disk-full --check pass --held yes)"
want "and it names both alternatives"      "or --pass" "$(sop applied disk-full --check pass --held yes)"
is "an unspellable --check is refused"     "1" "$(sop_rc applied disk-full --bead sp-t1 --check maybe --held yes)"
is "an unspellable --held is refused"      "1" "$(sop_rc applied disk-full --bead sp-t1 --check pass --held sortof)"
is "--check fail --held yes is refused"    "1" "$(sop_rc applied disk-full --bead sp-t1 --check fail --held yes)"
want "and it says why that cannot be true" "can have held" \
     "$(sop applied disk-full --bead sp-t1 --check fail --held yes)"
# A flag whose value is missing must not spin: `shift 2` with one argument left shifts
# nothing, and the parse loop would run forever on the same token.
is "a flag with no value is refused rather than hanging" "1" \
   "$(sop_rc applied disk-full --bead)"

n_after="$(wc -l < "$LEDGER")"
is "and not one refusal wrote a line" "$((n_before + 1))" "$n_after"

echo
echo "--- --pass: beadless sweep records"

# A SWEEP PASS HAS NO BEAD. The join target is the pass id — stable within one aeon run,
# so two ledger lines from one pass can be correlated. The ledger carries "pass" (not "bead"),
# and log --pass filters on it. The bead note is omitted (note=n/a).
PASS_ID="sweep-$(date -u +%s)-testops"
out="$(sop applied disk-full --pass "$PASS_ID" --check pass --held yes)"
is  "recording with --pass exits 0"                   "0" "$(sop_rc applied disk-full --pass "$PASS_ID" --check pass --held unknown)"
want "it says what it recorded"                        "recorded sop-disk-full on pass=$PASS_ID" "$out"

pass_line="$(grep "\"pass\"" "$LEDGER" | tail -1)"
want "the ledger line carries the pass key"   '"pass"'   "$pass_line"
nowant "and not the bead key"                 '"bead"'   "$pass_line"
want "shelf is still verified"                '"shelf":"ok"' "$pass_line"
want "note is n/a — no bead to annotate"      '"note":"n/a"' "$pass_line"

is "log --pass finds it"           "0" "$(sop_rc log --pass "$PASS_ID")"
is "log --bead does NOT find it"   "1" "$(sop_rc log --bead "$PASS_ID")"
want "log --pass prints the line"  "$PASS_ID" "$(sop log --pass "$PASS_ID")"
is "two records share a pass id"   "2" "$(sop log --pass "$PASS_ID" | wc -l)"

# MISSING BOTH IS STILL REFUSED: the reasoning that a record nobody can join back to
# something counts nothing still applies — only the join target has changed.
is "missing both --bead and --pass still refused" "1" "$(sop_rc applied disk-full --check pass --held yes)"

echo
echo "--- the record survives the day the harness itself is broken"

# THE SHELF CANNOT BE READ, so the slug cannot be verified and the note cannot be written.
# Refusing here would mean the one incident where the database is down is the one incident
# that leaves no trace, so the line is written anyway and says which half is missing.
SPIRA_DB_OVERRIDE="$TMP/no-such-database"
out="$(sop applied disk-full --bead sp-t1 --check pass --held unknown)"
rc="$(sop_rc applied disk-full --bead sp-t1 --check pass --held unknown)"
unset SPIRA_DB_OVERRIDE
tail2="$(tail -2 "$LEDGER")"
want "an unreadable shelf is recorded as unreadable" '"shelf":"unreadable"' "$tail2"
want "and the missing note is recorded as missing"   '"note":"failed"'      "$tail2"
is   "and the command exits non-zero about it"       "1" "$rc"
want "and says the human will not see it on the bead" "was NOT" "$out"

echo
echo "--- --why: full on the bead, bounded in the ledger"

long="$(printf 'x%.0s' $(seq 1 200))"
printf 'the volume filled because %s\n' "$long" | sop applied disk-full --bead sp-t1 --check pass --held no --why - >/dev/null 2>&1
w="$(python3 -c '
import sys, json
print(json.loads(open(sys.argv[1]).read().strip().splitlines()[-1])["why"])' "$LEDGER")"
is   "the ledger truncates why to the CONFIGURED cap" "$WHY_CAP" "${#w}"
want "keeping the front of it"                        "the volume filled" "$w"
want "and the bead keeps the whole sentence"          "${long:0:120}" "$(notes sp-t1)"
want "held=no reads as the runbook needing work"      "needs amending" "$(notes sp-t1)"

echo
echo "--- a corrupt ledger is not an empty one"

CORRUPT="$TMP/corrupt.jsonl"; printf 'this is not json\nnor is this\n' > "$CORRUPT"
LEDGER_OVERRIDE="$CORRUPT"
is "a ledger of unparseable lines reads 2, not 1" "2" "$(sop_rc log --bead sp-t1)"
want "and says the ledger is corrupt rather than empty" "corrupt, not empty" "$(sop log --bead sp-t1)"
unset LEDGER_OVERRIDE
# THE POSITIVE CONTROL FOR THAT 2: the same read against the good ledger still finds records,
# so the 2 above is the corruption being detected and not this suite having lost its way to
# the file.
is "and the good ledger still reads 0" "0" "$(sop_rc log --bead sp-t1)"

echo
echo "--- the brief requires it, between the CHECK and the FIX"

# THE POSITIVE CONTROL IS A DOCTORED COPY. A grep over a brief that finds what it wants tells
# you nothing until you have watched it fail on a brief that does not have it — which is the
# exact shape of the defect that let four personas ship naming eight commands that did not
# exist while the suite stayed green.
requires_record() {      # requires_record <file> -> "yes" | why not
    python3 - "$1" <<'PY'
import re, sys
t = open(sys.argv[1]).read()
# Step 1 is the loop's matching step; the rule is about where the record sits inside it.
m = re.search(r"^1\.(.*?)^2\.", t, re.S | re.M)
if not m: print("no step 1 in this brief"); raise SystemExit
s = m.group(1)
i_check = s.find("**CHECK**")
i_rec   = s.find("applied <slug>")
i_fix   = s.find("**FIX**")
if i_rec < 0: print("step 1 never tells the aeon to record the application"); raise SystemExit
if not (0 <= i_check < i_rec < i_fix):
    print("the record is not between the CHECK and the FIX (check=%d record=%d fix=%d)"
          % (i_check, i_rec, i_fix)); raise SystemExit
for f in ("--bead", "--check", "--held"):
    if f not in s: print("step 1 does not name %s" % f); raise SystemExit
print("yes")
PY
}
is "ops.md requires the record between CHECK and FIX" "yes" "$(requires_record "$HERE/chamber/ops.md")"
want "and states held=yes as a real outcome" "held, and it taught us nothing new" "$(cat "$HERE/chamber/ops.md")"

DOCTORED="$TMP/ops-without.md"
python3 - "$HERE/chamber/ops.md" "$DOCTORED" <<'PY'
import re, sys
t = open(sys.argv[1]).read()
# Remove every line that mentions the record, which is exactly what a future edit that
# quietly drops the requirement would look like.
out = "\n".join(l for l in t.split("\n") if "applied <slug>" not in l)
open(sys.argv[2], "w").write(out)
PY
nowant "the check FAILS on a brief with the requirement removed" "yes" "$(requires_record "$DOCTORED")"

# AND THE PATH IT NAMES MUST RESOLVE. `{{SOP}}` is substituted with the harness's own sop.sh,
# so the brief telling an aeon to run `applied` is only worth anything if that subcommand is
# there — a brief naming a subcommand this program does not have fails at 3am, not here.
is "the subcommand the brief names exists" "0" "$(sop_rc log --bead sp-t1)"
want "and 'applied' is in the program's own usage" "sop.sh applied" "$(sop bogus-subcommand)"

echo
echo "--- digest: what the shelf holds, so a write can be seen after the fact"

# A DIGEST EXISTS BECAUSE `bd remember` UPSERTS. Amending a runbook leaves the shelf exactly
# the size it was, so a caller asking "did this session leave a runbook behind" cannot count
# and cannot compare a whole-shelf hash either — that would read a RETIREMENT as a write.
# Every assertion here is about that distinction.
d0="$(sop digest)"
want "digest names the SOP on the shelf" "sop-disk-full" "$d0"
is   "one line per SOP"                  "1" "$(printf '%s\n' "$d0" | grep -c .)"
is   "and it reads 0"                    "0" "$(sop_rc digest)"

sop write disk-full - <<'SOP' >/dev/null 2>&1
MATCH: (No space left on device|disk.*full)
SYMPTOM: a unit fails and the volume it writes to is full
CHECK: df -h /var | tail -1
FIX: clear the oldest artifacts, then restart the unit, then verify the next run is green
SOP
d1="$(sop digest)"
is     "an AMENDED SOP leaves the shelf the same size" "1" "$(printf '%s\n' "$d1" | grep -c .)"
nowant "but its line changed, so the amendment is visible" "$d1" "$d0"

sop write clock-skew - <<'SOP' >/dev/null 2>&1
SYMPTOM: a unit fails because the box's clock moved
CHECK: timedatectl show -p NTPSynchronized
FIX: restart the time sync unit
SOP
d2="$(sop digest)"
is   "a NEW SOP adds a line"                   "2" "$(printf '%s\n' "$d2" | grep -c .)"
is   "and loses no earlier line to it"         "0" "$(comm -23 <(printf '%s\n' "$d1" | sort) <(printf '%s\n' "$d2" | sort) | grep -c .)"
is   "so exactly one line is new"              "1" "$(comm -13 <(printf '%s\n' "$d1" | sort) <(printf '%s\n' "$d2" | sort) | grep -c .)"

# A RETIREMENT IS NOT A WRITE, and this is the assertion that makes the format load-bearing:
# after retiring, no line exists that was absent before, which is the test a caller runs.
sop retire clock-skew >/dev/null 2>&1
d3="$(sop digest)"
is "after a retirement no line is new" "0" \
   "$(comm -13 <(printf '%s\n' "$d2" | sort) <(printf '%s\n' "$d3" | sort) | grep -c .)"
is "and the shelf shrank"              "1" "$(printf '%s\n' "$d3" | grep -c .)"

# THE POSITIVE CONTROL FOR THE 2. An unreadable shelf must not read as an empty one, or a
# database outage looks exactly like a session that wrote nothing.
SPIRA_DB_OVERRIDE="$TMP/no-such-db"
is   "an unreadable shelf reads 2, not 0"          "2" "$(sop_rc digest)"
want "and says so rather than printing an empty shelf" "not an empty shelf" "$(sop digest)"
unset SPIRA_DB_OVERRIDE
is   "and the real shelf still reads 0"            "0" "$(sop_rc digest)"

echo
echo "--- log --check and --since: which records, and whose"

# The closing-rule check asks a narrower question than "has anything ever been recorded
# against this bead": it asks whether THIS session recorded that a runbook actually fitted.
# Both filters exist for that, and both keep the three-valued exit.
sop applied disk-full --bead sp-t2 --check fail --held unknown >/dev/null 2>&1
is "a bead with only a check=fail record reads 1 for pass" "1" "$(sop_rc log --bead sp-t2 --check pass)"
is "and 0 for fail"                                        "0" "$(sop_rc log --bead sp-t2 --check fail)"
is "an unspellable --check is refused"                     "1" "$(sop_rc log --bead sp-t2 --check maybe)"
is "a non-numeric --since is refused rather than read as 0" "1" "$(sop_rc log --bead sp-t1 --since yesterday)"

now="$(date -u +%s)"
is "records made before a --since window are not in it" "1" "$(sop_rc log --bead sp-t1 --since $((now + 60)))"
is "and the same read without the window still finds them" "0" "$(sop_rc log --bead sp-t1)"
sop applied disk-full --bead sp-t1 --check pass --held yes >/dev/null 2>&1
is "a record made inside the window is in it" "0" "$(sop_rc log --bead sp-t1 --since $((now - 5)))"

echo
echo "--- ledger-init: absence has to be observable before it is acted on"

# WITHOUT THIS, A FRESH INSTALL CANNOT DISTINGUISH "nothing was recorded" FROM "no ledger",
# and the closing-rule check would decline to judge precisely the sessions it exists to
# catch — the ones that recorded nothing, on a shelf nobody had recorded against yet.
FRESH="$TMP/fresh/applications.jsonl"
LEDGER_OVERRIDE="$FRESH"
is   "with no ledger at all, a read is UNREADABLE"  "2" "$(sop_rc log --bead sp-t1)"
want "ledger-init says it created one"              "created empty ledger" "$(sop ledger-init)"
is   "and now the same read is a TRUE absence"      "1" "$(sop_rc log --bead sp-t1)"
want "running it again leaves the existing one alone" "ledger present" "$(sop ledger-init)"
LEDGER_OVERRIDE=/proc/nope/applications.jsonl
is   "an unwritable ledger path fails rather than pretending" "1" "$(sop_rc ledger-init)"
unset LEDGER_OVERRIDE

echo
echo "--- match: sweep SOP fires on a real payload, scores via MATCH not key-tokens"

# A REAL SWEEP PAYLOAD excerpted from a live incident's bd-show output (sp-t3dc,
# 2026-09-07T23:50Z), not invented. The bead title and the watchtower body come from
# the same template, so this fixture reproduces the form the matcher actually receives.
# Using real output rather than synthetic text prevents a test that passes on words the
# template does not generate (law-fixtures-carry-real-cadence).
SWEEP_PAYLOAD='Spira sweep — is the pipeline moving?   [● P1 · CLOSED]
Type: bug

DESCRIPTION

  ## Spira pipeline, 2026-09-07T23:50:17Z

  N workers pull from a DAG into a merge queue. These are that queue'"'"'s vital
  signs. A field reading ? is one this pass COULD NOT READ.

  ### The far end — is anything coming out?

  minutes since the last landing      7
  branches finished but not landed    0

  ### The workers

  aeons alive                         3
  ready to claim                      85'

# A PAYLOAD FROM AN UNRELATED INCIDENT — a unit failure with no sweep content.
# Using a heredoc so hermetic.sh skips the body (it contains "systemctl" as data, not a call).
read -r -d '' UNRELATED_PAYLOAD <<'PAYLOAD' || true
unit failed: mtgc-alert-prod@1.service   [● P2 · OPEN]
Type: bug

DESCRIPTION

  systemctl status: failed (ExitCode=1)
  Journal: connection refused on port 5432
PAYLOAD

# Write the sweep SOP with the deployed MATCH regex to the fixture database. This is
# what controls exactly what the matcher sees — the test does NOT read the live database,
# which would be reading the state of this box.
sop write spira-sweep - <<'SOP' >/dev/null 2>&1
MATCH: Spira sweep — is the pipeline moving|minutes since the last landing
SYMPTOM: the ten-minute watchtower sweep — vital signs, not a failure.
CHECK: read the bead metadata as procedure, not as severity signal.
FIX: check landstate, check workers, commit before close.
SOP

# THE POSITIVE CONTROL COMES FIRST: verify the sweep SOP is on the fixture shelf
# before testing that match finds it. A shelf missing the SOP looks identical to a
# broken matcher — only the positive control tells them apart.
want "the sweep SOP is on the fixture shelf" "sop-spira-sweep" "$(sop list)"

sweep_match="$(printf '%s\n' "$SWEEP_PAYLOAD" | sop match -)"
unrel_match="$(printf '%s\n' "$UNRELATED_PAYLOAD" | sop match -)"

want "sop-spira-sweep fires on a real sweep payload" "sop-spira-sweep" "$sweep_match"
want "it scores via MATCH, not key-tokens"           "MATCH"           "$sweep_match"
nowant "it does NOT fire on an unrelated payload"    "sop-spira-sweep" "$unrel_match"

# THE POSITIVE CONTROL FOR THE NEGATIVE: the disk-full SOP fires on a payload that
# names disk-full symptoms, proving the matcher works. If it could not find a hit even
# here, the negative above would be a broken matcher reporting silence.
disk_payload='No space left on device — df -h shows /var at 100%'
disk_match="$(printf '%s\n' "$disk_payload" | sop match -)"
want "disk-full fires on its own payload (positive control for the negative)" "sop-disk-full" "$disk_match"
nowant "disk-full does not fire on a sweep payload" "sop-disk-full" "$sweep_match"

echo
echo "--- METRIC: held=yes is only allowed when the metric has cleared"

# THE MECHANIC UNDER TEST. A SOP with METRIC: <KEY> <SUBCMD> declares that the named
# cockpit metric must reach 0 before held=yes can be recorded. sop.sh applied re-reads
# the metric via cockpit.sh <SUBCMD> on every held=yes attempt; if the value is still
# nonzero or unreadable, held is downgraded to unknown automatically.
#
# This is the enforcement that prevents the ledger from recording held=yes for a fix that
# did not clear the metric (sp-gt7k): a CHECK that returned zero because it looked in the
# wrong place could not trigger a downgrade because sop.sh never re-read the real number.
#
# THE MOCK COCKPIT stands in for cockpit.sh in the clean test environment. It outputs
# SP_UNADOPTED=$MOCK_UNADOPTED for the "unsent" subcommand and nothing for others.
# SOP_METRIC_COCKPIT overrides which binary sop.sh calls; the default ($HERE/cockpit.sh)
# would not run in env -i without SPIRA_HOME and friends, so every METRIC test uses sop_m.
MOCK_COCKPIT="$TMP/mock-cockpit"
cat > "$MOCK_COCKPIT" << 'MOCK'
#!/usr/bin/env bash
case "${1:-}" in
    unsent) printf 'SP_UNADOPTED=%s\n' "${MOCK_UNADOPTED:-0}" ;;
    *) true ;;
esac
MOCK
chmod +x "$MOCK_COCKPIT"

sop_m() {   # like sop() but with SOP_METRIC_COCKPIT — defaults to the mock, overridable.
    # The outer SOP_METRIC_COCKPIT variable overrides $MOCK_COCKPIT so that individual tests
    # can substitute a different cockpit (e.g. a silent one) without changing $MOCK_COCKPIT.
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_RUN="$RUN" \
        SPIRA_DB="${SPIRA_DB_OVERRIDE:-$SPIRA_DB}" \
        SPIRA_SOP_LEDGER="${LEDGER_OVERRIDE:-$LEDGER}" SOP_WHY_CAP="$WHY_CAP" \
        BEADS_ACTOR="aeon-testops" BEADS_NO_AUTO_IMPORT=1 \
        SOP_METRIC_COCKPIT="${SOP_METRIC_COCKPIT:-$MOCK_COCKPIT}" \
        MOCK_UNADOPTED="${MOCK_UNADOPTED:-0}" \
        timeout 120 bash "$HERE/sop.sh" "$@" 2>&1
}
sop_m_rc() { sop_m "$@" >/dev/null 2>&1; printf '%s' "$?"; }

# Write a SOP that declares METRIC: SP_UNADOPTED unsent. The CHECK already calls the
# detector; the METRIC field tells applied which number to re-confirm before held=yes.
sop write metric-sop - <<'SOP' >/dev/null 2>&1
MATCH: SP_UNADOPTED.*nonzero|unadopted refs
SYMPTOM: unadopted refs remain after cleanup
CHECK: bash $SPIRA_HOME/cockpit.sh unsent 2>/dev/null | grep "^SP_UNADOPTED="
METRIC: SP_UNADOPTED unsent
FIX: remove the stray branches with git -C <repo> branch -D <branch>
SOP

want "the metric SOP is on the shelf" "sop-metric-sop" "$(sop list)"

# POSITIVE CONTROL: verify the mock itself works before trusting the downgrade tests.
# A broken mock that outputs nothing every time would make every "held=unknown" test
# pass for the wrong reason (the cockpit returned nothing → unknown, regardless of the fix).
mock_out="$(MOCK_UNADOPTED=3 bash "$MOCK_COCKPIT" unsent)"
is "mock cockpit outputs the configured value" "SP_UNADOPTED=3" "$mock_out"

# WITH METRIC NONZERO: held=yes must be downgraded to unknown automatically.
# This is the defect this bead closes: 17 held=yes entries while SP_UNADOPTED was still 1.
MOCK_UNADOPTED=1 sop_m applied metric-sop --bead sp-t1 --check pass --held yes >/dev/null 2>&1
heldval="$(python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
last = [r for r in rows if r.get("sop")=="sop-metric-sop" and r.get("bead")=="sp-t1"]
print(last[-1]["held"] if last else "none")
' "$LEDGER" 2>/dev/null)"
is "held=yes downgraded to unknown when SP_UNADOPTED=1" "unknown" "$heldval"

# WITH METRIC ZERO: held=yes is allowed — the fix genuinely cleared the metric.
MOCK_UNADOPTED=0 sop_m applied metric-sop --bead sp-t2 --check pass --held yes >/dev/null 2>&1
heldval2="$(python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
last = [r for r in rows if r.get("sop")=="sop-metric-sop" and r.get("bead")=="sp-t2"]
print(last[-1]["held"] if last else "none")
' "$LEDGER" 2>/dev/null)"
is "held=yes allowed when SP_UNADOPTED=0" "yes" "$heldval2"

# COCKPIT RETURNS ?: held=yes becomes unknown — cannot confirm from a failed probe.
MOCK_UNADOPTED='?' sop_m applied metric-sop --bead sp-t1 --check pass --held yes >/dev/null 2>&1
heldval3="$(python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
last = [r for r in rows if r.get("sop")=="sop-metric-sop" and r.get("bead")=="sp-t1"]
print(last[-1]["held"] if last else "none")
' "$LEDGER" 2>/dev/null)"
is "held=yes is unknown when cockpit returns ?" "unknown" "$heldval3"

# COCKPIT UNAVAILABLE (empty output): held=yes becomes unknown.
cat > "$TMP/silent-cockpit" << 'SILENT'
#!/usr/bin/env bash
true
SILENT
chmod +x "$TMP/silent-cockpit"
SOP_METRIC_COCKPIT="$TMP/silent-cockpit" MOCK_UNADOPTED=0 sop_m applied metric-sop --bead sp-t1 --check pass --held yes >/dev/null 2>&1
heldval4="$(python3 -c '
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
last = [r for r in rows if r.get("sop")=="sop-metric-sop" and r.get("bead")=="sp-t1"]
print(last[-1]["held"] if last else "none")
' "$LEDGER" 2>/dev/null)"
is "held=yes is unknown when cockpit returns nothing" "unknown" "$heldval4"

# SOPs WITHOUT METRIC: held=yes is still allowed normally — METRIC is opt-in.
is "held=yes unaffected on SOP without METRIC" "0" \
   "$(sop_rc applied disk-full --bead sp-t2 --check pass --held yes)"

# THE BEAD NOTE EXPLAINS THE DOWNGRADE. A human reading the incident bead should see
# why held was not yes — the metric value and what it means.
note_sp_t1="$(notes sp-t1)"
want "note explains downgrade: names the METRIC key" "METRIC" "$note_sp_t1"

# LINT ACCEPTS A WELL-FORMED METRIC FIELD and catches a bad one written via the back door.
# `sop write` does not yet validate METRIC format; `lint` is what closes the gap.
sop write good-metric - <<'SOP' >/dev/null 2>&1
SYMPTOM: test
CHECK: check
FIX: fix
METRIC: SP_UNADOPTED unsent
SOP
is "lint accepts a well-formed METRIC field" "0" "$(sop_rc lint)"

# Write a malformed METRIC via the back door (bypassing `sop write`) — the same route
# that could produce a broken SOP in production.  Lint must name it.
bdt remember --key sop-bad-metric "$(printf 'SYMPTOM: t\nCHECK: c\nFIX: f\nMETRIC: notakey notasubcmd123$')" >/dev/null 2>&1
is  "lint catches a malformed METRIC via the back door" "1" "$(sop_rc lint)"
want "lint names METRIC as the problem"               "METRIC" "$(sop lint)"
bdt forget sop-bad-metric >/dev/null 2>&1
is  "lint is clean after removing the bad METRIC sop" "0"     "$(sop_rc lint)"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
