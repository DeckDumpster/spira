#!/usr/bin/env bash
#
# test-sop.sh — an SOP is validated the same way whether it arrives through `write` or the
#               back door, and an application leaves a record DISTINGUISHABLE from silence.
#
# TWO CLAIMS.
#
# THE VALIDATOR (UC-operator-channel-44) — `sop.sh validate <slug>`, body on stdin, no shelf
# and no bd: the same rules `write` enforces and `lint` re-checks against the whole shelf.
# One T1 table drives every rule (SYMPTOM/CHECK/FIX, MATCH, word cap, METRIC shape, no
# operator-specific path), each planted violation alone so "one rule, one violation" is an
# assertion and not a hope. This absorbs test-sop-lint.sh and the METRIC-only plant/forget
# this file used to carry beside it (D9): one table, not two files doing the same thing two
# different ways.
#
# THE LEDGER (UC-operator-channel-45) — `sop.sh applied` records that a runbook was matched
# and what came of it, to a ledger and to a bead note, and the property under test is a
# DISTINCTION, not a value:
#
#   the ledger was read and holds a record for this bead          exit 0, the line on stdout
#   the ledger was read and holds nothing for this bead           exit 1, a TRUE absence
#   the ledger could not be read at all                           exit 2, NOT an absence
#
# A two-valued answer would merge the second and third, and the merged one reads as
# all-clear (law-absence-needs-a-positive-control). So every assertion below that something
# is ABSENT is preceded, in the same fixture, by proof that the same read finds the same
# thing when it is present, and the unreadable cases are asserted as 2 rather than as 1.
#
# A REAL `bd` ON A THROWAWAY DATABASE for the ledger half. Half of what `applied` must do is
# put a note on a bead, and a stub `bd` would reproduce the surface this suite remembers
# rather than the one `sop.sh` actually calls (law-prefer-the-real-dependency). SOP_SHELF_CMD
# stands in for the shelf read specifically — the ONE thing a fixture can answer as well as a
# real database — so only the bead-note cases need bd.
#
# THE ENVIRONMENT IS EXPLICIT AND MINIMAL, and SPIRA_SOP_LEDGER and SOP_WHY_CAP ARE PINNED TO
# NON-DEFAULTS. A suite that inherits a real spira.conf asserts against one box, one that
# inherits SPIRA_WIKI writes into a real wiki page, and one that asserts against the shipped
# ledger path passes just as well if the code has that path written in — which is the thing
# the key exists to stop (law-gates-run-in-a-clean-environment).
#
# defect: sp-9p1a sp-atts
# covers: spira/sop.sh spira/chamber/ops.md spira/chamber/* UC-operator-channel-44 UC-operator-channel-45
# scar: nothing recorded that an SOP was matched and applied; a session that ignored a runbook left the same trace as one that executed it faithfully. bd remember sop-<slug> bypassed sop.sh write's validator; a 236-word prose SOP with no structured fields was stored and was findable only by weak key-token fallback.
# tier: T2
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# ===========================================================================================
echo "--- the validator: one rule, one violation, no shelf and no bd ---"
# ===========================================================================================

# sop_v <slug> <body> -> validates via the write-time validator, body on stdin.
sop_v() { printf '%s' "$2" | env -i HOME="$HOME" PATH="$PATH" bash "$HERE/sop.sh" validate "$1" 2>&1; }
sop_v_rc() { sop_v "$1" "$2" >/dev/null 2>&1; printf '%s' "$?"; }

WELL_FORMED="$(printf 'MATCH: (test)\nSYMPTOM: test failed\nCHECK: check it\nFIX: fix it')"
is "a well-formed SOP passes"           "0" "$(sop_v_rc well-formed "$WELL_FORMED")"
want "and says so"                      "ok" "$(sop_v well-formed "$WELL_FORMED")"

# PLANT A VALID SOP ALONGSIDE EACH BAD ONE (in the assertion text, not the shelf — there is
# no shelf here): "all fail" and "only the right one fails" must be distinguishable.
NO_CHECK="$(printf 'SYMPTOM: something is wrong\nFIX: restart the unit')"
out_nc="$(sop_v no-check "$NO_CHECK")"
is     "missing CHECK is caught"              "1"     "$(sop_v_rc no-check "$NO_CHECK")"
want   "CHECK is named as missing"            "CHECK" "$out_nc"
nowant "SYMPTOM not named (it is present)"    "SYMPTOM" "$out_nc"
nowant "FIX not named (it is present)"        "FIX"     "$out_nc"

NO_FIELDS="This SOP was written as a prose blob with no structured fields at all and no MATCH line."
out_nf="$(sop_v no-fields "$NO_FIELDS")"
is   "a prose blob with no fields fails"  "1"        "$(sop_v_rc no-fields "$NO_FIELDS")"
want "it names missing SYMPTOM"           "SYMPTOM"  "$out_nf"
want "it names missing CHECK"             "CHECK"    "$out_nf"
want "it names missing FIX"               "FIX"      "$out_nf"

BAD_REGEX="$(printf 'MATCH: ([unclosed\nSYMPTOM: something bad\nCHECK: check something\nFIX: fix something')"
is   "invalid MATCH regex is caught"        "1"             "$(sop_v_rc bad-regex "$BAD_REGEX")"
want "names it as a regex problem"          "extended regex" "$(sop_v bad-regex "$BAD_REGEX")"

OVER_CAP="$(python3 -c 'print("SYMPTOM: the service failed " + " ".join(["filler"]*250) + "\nCHECK: check it\nFIX: fix it")')"
is   "over word cap is caught"   "1"     "$(sop_v_rc too-long "$OVER_CAP")"
want "names the word count"      "words" "$(sop_v too-long "$OVER_CAP")"

GOOD_METRIC="$(printf 'SYMPTOM: t\nCHECK: c\nFIX: f\nMETRIC: SP_UNADOPTED unsent')"
is "a well-formed METRIC field passes"  "0" "$(sop_v_rc good-metric "$GOOD_METRIC")"

BAD_METRIC="$(printf 'SYMPTOM: t\nCHECK: c\nFIX: f\nMETRIC: notakey notasubcmd123$')"
out_bm="$(sop_v bad-metric "$BAD_METRIC")"
is   "a malformed METRIC is caught"    "1"       "$(sop_v_rc bad-metric "$BAD_METRIC")"
want "METRIC is named as the problem"  "METRIC"  "$out_bm"

is "an empty body is caught" "1" "$(sop_v_rc empty "")"
want "and says so" "empty SOP" "$(sop_v empty "")"

# THE OPERATOR-PATH FENCE. Split so this file's own source is not itself flagged.
_bad_prefix="/"'home'"/test-spira/"
BAD_PATH="$(printf 'SYMPTOM: a unit failed\nCHECK: ls %sdb\nFIX: fix it' "$_bad_prefix")"
out_bp="$(sop_v bad-path "$BAD_PATH")"
is   "a SOP naming an absolute home path is refused"  "1"           "$(sop_v_rc bad-path "$BAD_PATH")"
want "and names the offending token"                  "$_bad_prefix" "$out_bp"

GOOD_PATH="$(printf 'SYMPTOM: a unit failed\nCHECK: ls \$SPIRA_DB\nFIX: fix it')"
is "the same shape using an env var passes" "0" "$(sop_v_rc good-path "$GOOD_PATH")"

echo
echo "--- lint: the validator applied to the whole shelf, via SOP_SHELF_CMD ---"

# SOP_SHELF_CMD IS THE SEAM: its output stands in for `bd memories --json`. No database
# anywhere in this section — including the fail-closed case, where the command simply
# produces nothing, exactly as a broken bd read does.
sop_shelf() { env -i HOME="$HOME" PATH="$PATH" SOP_SHELF_CMD="$1" bash "$HERE/sop.sh" "${@:2}" 2>&1; }
sop_shelf_rc() { sop_shelf "$@" >/dev/null 2>&1; printf '%s' "$?"; }

CLEAN_SHELF='printf "%s" "{\"sop-clean\":\"SYMPTOM: s\\nCHECK: c\\nFIX: f\"}"'
is   "lint over a clean fixture shelf exits 0"  "0"  "$(sop_shelf_rc "$CLEAN_SHELF" lint)"
want "and reports it valid"                     "ok" "$(sop_shelf "$CLEAN_SHELF" lint)"

DIRTY_SHELF='printf "%s" "{\"sop-clean\":\"SYMPTOM: s\\nCHECK: c\\nFIX: f\",\"sop-broken\":\"no fields\"}"'
out_dirty="$(sop_shelf "$DIRTY_SHELF" lint)"
is   "lint over a shelf with one bad SOP exits 1"    "1"           "$(sop_shelf_rc "$DIRTY_SHELF" lint)"
want "it names the failing key"                      "sop-broken"  "$out_dirty"
nowant "and leaves the clean key unmentioned as a failure" "FAIL  sop-clean" "$out_dirty"

# THE ONE FAIL-CLOSED CASE (UC-44's T2): the shelf command fails outright, exactly like an
# unreachable database. lint must refuse to report clean, not read failure as an empty shelf.
is   "an unreadable shelf exits non-zero"                "1" "$(sop_shelf_rc "false" lint)"
want "and refuses to report clean"  "refusing to report clean" "$(sop_shelf "false" lint)"

# ===========================================================================================
echo
echo "--- the ledger, against a real bd (bead notes need one) ---"
# ===========================================================================================

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sop
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sop || bail "could not build a fixture database"

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
sop_rc() {               # the same, but the caller wants only the status
    sop "$@" >/dev/null 2>&1; printf '%s' "$?"
}
# sop_run <args...> — ONE invocation; sets $SOP_OUT (stdout+stderr) and $SOP_RC (exit status),
# so an assertion needing both does not pay for the command twice.
SOP_OUT=""; SOP_RC=0
sop_run() { SOP_OUT="$(sop "$@")"; SOP_RC=$?; }
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
echo "--- write refuses an absolute operator-specific path (the same fence, live) ---"

_live_bad_path="/"'home'"/test-spira/db"
_live_bad_prefix="/"'home'"/test-spira/"
bad_path_sop="$(printf 'SYMPTOM: a unit failed\nCHECK: ls %s\nFIX: fix it' "$_live_bad_path")"
sop_run write bad-path-sop - <<< "$bad_path_sop"
is   "write refuses a SOP with an absolute home path"  "1" "$SOP_RC"
want "and names the offending token" "$_live_bad_prefix" "$SOP_OUT"
want "and says to use env vars"      "env var"           "$SOP_OUT"
is   "the bad SOP was not stored"    "1" "$(bdt recall sop-bad-path-sop 2>/dev/null; printf '%s' "$?")"

good_path_sop="$(printf 'SYMPTOM: a unit failed\nCHECK: ls \$SPIRA_DB\nFIX: fix it')"
is  "write accepts a SOP using env vars"   "0" "$(sop_rc write good-path-sop - <<< "$good_path_sop")"
want "and it is on the shelf"  "sop-good-path-sop" "$(sop list)"
bdt forget sop-good-path-sop >/dev/null 2>&1

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

sop_run applied disk-full --bead sp-t1 --check pass --held yes
is   "recording exits 0"                         "0" "$SOP_RC"
want "it says what it recorded"                  "recorded sop-disk-full on sp-t1" "$SOP_OUT"
want "--held yes reads as a complete outcome"    "taught us nothing new" "$SOP_OUT"

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

sop_run log --bead sp-t1
is   "a bead with a record reads 0"          "0"     "$SOP_RC"
want "and the one with a record prints it"   "sp-t1" "$SOP_OUT"
is "a bead with NO record reads 1"         "1" "$(sop_rc log --bead sp-t2)"
nowant "the unrecorded bead prints nothing" "sp-t2" "$SOP_OUT"
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
# and log --pass filters on it. The bead note is omitted (note=n/a). TWO DISTINCT --held
# values are recorded deliberately here (yes, then unknown) — the count below depends on
# there being exactly two records under this pass id.
PASS_ID="sweep-$(date -u +%s)-testops"
sop_run applied disk-full --pass "$PASS_ID" --check pass --held yes
is  "recording with --pass exits 0"     "0" "$SOP_RC"
want "it says what it recorded"         "recorded sop-disk-full on pass=$PASS_ID" "$SOP_OUT"
sop applied disk-full --pass "$PASS_ID" --check pass --held unknown >/dev/null 2>&1

pass_line="$(grep "\"pass\"" "$LEDGER" | tail -1)"
want "the ledger line carries the pass key"   '"pass"'   "$pass_line"
nowant "and not the bead key"                 '"bead"'   "$pass_line"
want "shelf is still verified"                '"shelf":"ok"' "$pass_line"
want "note is n/a — no bead to annotate"      '"note":"n/a"' "$pass_line"

is "log --pass finds it"           "0" "$(sop_rc log --pass "$PASS_ID")"
is "log --bead does NOT find it"   "1" "$(sop_rc log --bead "$PASS_ID")"
want "log --pass prints the line"  "$PASS_ID" "$(sop log --pass "$PASS_ID")"
is "two records share a pass id"   "2" "$(sop log --pass "$PASS_ID" | wc -l)"

echo
echo "--- the record survives the day the harness itself is broken"

# THE SHELF CANNOT BE READ, so the slug cannot be verified and the note cannot be written.
# Refusing here would mean the one incident where the database is down is the one incident
# that leaves no trace, so the line is written anyway and says which half is missing.
SPIRA_DB_OVERRIDE="$TMP/no-such-database"
sop_run applied disk-full --bead sp-t1 --check pass --held unknown
unset SPIRA_DB_OVERRIDE
tail1="$(tail -1 "$LEDGER")"
want "an unreadable shelf is recorded as unreadable" '"shelf":"unreadable"' "$tail1"
want "and the missing note is recorded as missing"   '"note":"failed"'      "$tail1"
is   "and the command exits non-zero about it"       "1" "$SOP_RC"
want "and says the human will not see it on the bead" "was NOT" "$SOP_OUT"

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
sop_run digest
d0="$SOP_OUT"
want "digest names the SOP on the shelf" "sop-disk-full" "$d0"
is   "one line per SOP"                  "1" "$(printf '%s\n' "$d0" | grep -c .)"
is   "and it reads 0"                    "0" "$SOP_RC"

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
sop_run digest
unset SPIRA_DB_OVERRIDE
is   "an unreadable shelf reads 2, not 0"          "2" "$SOP_RC"
want "and says so rather than printing an empty shelf" "not an empty shelf" "$SOP_OUT"
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
read -r -d '' UNRELATED_PAYLOAD <<'PAYLOAD' || true
unit failed: mtgc-alert-prod@1.service   [● P2 · OPEN]
Type: bug

DESCRIPTION

  systemctl status: failed (ExitCode=1)
  Journal: connection refused on port 5432
PAYLOAD

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
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_RUN="$RUN" \
        SPIRA_DB="${SPIRA_DB_OVERRIDE:-$SPIRA_DB}" \
        SPIRA_SOP_LEDGER="${LEDGER_OVERRIDE:-$LEDGER}" SOP_WHY_CAP="$WHY_CAP" \
        BEADS_ACTOR="aeon-testops" BEADS_NO_AUTO_IMPORT=1 \
        SOP_METRIC_COCKPIT="${SOP_METRIC_COCKPIT:-$MOCK_COCKPIT}" \
        MOCK_UNADOPTED="${MOCK_UNADOPTED:-0}" \
        timeout 120 bash "$HERE/sop.sh" "$@" 2>&1
}

sop write metric-sop - <<'SOP' >/dev/null 2>&1
MATCH: SP_UNADOPTED.*nonzero|unadopted refs
SYMPTOM: unadopted refs remain after cleanup
CHECK: bash $SPIRA_HOME/cockpit.sh unsent 2>/dev/null | grep "^SP_UNADOPTED="
METRIC: SP_UNADOPTED unsent
FIX: remove the stray branches with git -C <repo> branch -D <branch>
SOP

want "the metric SOP is on the shelf" "sop-metric-sop" "$(sop list)"

# POSITIVE CONTROL: verify the mock itself works before trusting the downgrade tests.
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

tl_summary
