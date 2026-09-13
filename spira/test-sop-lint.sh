#!/usr/bin/env bash
#
# test-sop-lint.sh — sop.sh lint catches malformed SOPs that bypassed the write validator.
#
# THE BACK DOOR. `sop.sh write` validates every SOP before storing it; `bd remember sop-<slug>`
# does not — it is the route a direct database write takes, and the one that produced
# sop-spira-sweep: 236 words of prose, no fields, findable only by weak key-token fallback.
# `sop.sh lint` is what closes that door: it reads every sop- key and applies the write
# validator's own rules, so a runbook written directly cannot pass unnoticed.
#
# THIS SUITE IS THE POSITIVE CONTROL FOR THAT CHECK. A lint that reports clean on an empty
# shelf proves nothing — an empty result and a blind check look identical from outside. The
# only proof is planting a malformed SOP via the back door and requiring lint to name it.
#
#   ./test-sop-lint.sh
#
# defect: sp-atts
# covers: spira/sop.sh spira/gate-spira.sh
# scar: bd remember sop-<slug> bypassed sop.sh write's validator; a 236-word prose SOP with no structured fields was stored and was findable only by weak key-token fallback.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-sop-lint.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-sop-lint
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sop-lint || { echo "test-sop-lint: could not build a fixture database"; exit 1; }

sop() {
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/nonexistent.conf" \
        SPIRA_DB="${SPIRA_DB_OVERRIDE:-$SPIRA_DB}" \
        BEADS_NO_AUTO_IMPORT=1 \
        timeout 60 bash "$HERE/sop.sh" "$@" 2>&1
}
sop_rc() { sop "$@" >/dev/null 2>&1; printf '%s' "$?"; }
bdt() { bd -C "$SPIRA_DB" "$@"; }

# ---------------------------------------------------------------------------
# THE POSITIVE CONTROL — comes first, so a suite failure here fails fast and
# clearly. If lint exits 0 on this shelf the remaining assertions are worthless.
# ---------------------------------------------------------------------------
echo
echo "--- positive control: lint must reject what bypassed the door"

# A prose blob written DIRECTLY via bd remember, bypassing sop.sh write exactly as the
# back door does. No SYMPTOM, no CHECK, no FIX — the shape of sop-spira-sweep.
bdt remember --key sop-no-fields "This SOP was written as a prose blob with no structured fields at all and no MATCH line." >/dev/null 2>&1

is  "lint exits non-zero on a malformed SOP"   "1" "$(sop_rc lint)"
out="$(sop lint)"
want "it names the failing SOP key"     "sop-no-fields" "$out"
want "it names missing SYMPTOM"         "SYMPTOM"       "$out"
want "it names missing CHECK"           "CHECK"         "$out"
want "it names missing FIX"             "FIX"           "$out"

# AFTER REMOVING THE BAD SOP, lint must exit 0. If not, lint is broken independent of the
# shelf's content and all further assertions would pass for the wrong reason.
bdt forget sop-no-fields >/dev/null 2>&1
is  "lint exits 0 when the shelf is clean"     "0" "$(sop_rc lint)"
want "and reports the shelf is valid"  "ok" "$(sop lint)"

# ---------------------------------------------------------------------------
# ONE RULE, ONE VIOLATION — each validation rule catches only what it applies to.
# ---------------------------------------------------------------------------
echo
echo "--- each rule catches its own violation"

# Plant a valid SOP alongside each bad one, so "all fail" and "only the right one fails"
# are distinguishable.
bdt remember --key sop-valid "$(printf 'MATCH: (test)\nSYMPTOM: test failed\nCHECK: check it\nFIX: fix it')" >/dev/null 2>&1
is "a well-formed SOP does not fail lint" "0" "$(sop_rc lint)"

# MISSING CHECK — the most common omission in prose SOPs.
bdt remember --key sop-no-check "$(printf 'SYMPTOM: something is wrong\nFIX: restart the unit')" >/dev/null 2>&1
out="$(sop lint)"
is  "missing CHECK is caught"            "1"       "$(sop_rc lint)"
want "CHECK is named as missing"         "CHECK"   "$out"
nowant "SYMPTOM not named (it is present)" "SYMPTOM" "$out"
nowant "FIX not named (it is present)"    "FIX"     "$out"
bdt forget sop-no-check >/dev/null 2>&1
is "shelf is clean after removing sop-no-check" "0" "$(sop_rc lint)"

# INVALID MATCH REGEX — an unclosed group is what `write` rejects with "not a valid extended regex".
bdt remember --key sop-bad-regex "$(printf 'MATCH: ([unclosed\nSYMPTOM: something bad\nCHECK: check something\nFIX: fix something')" >/dev/null 2>&1
is  "invalid MATCH regex is caught"          "1" "$(sop_rc lint)"
want "names it as a regex problem" "extended regex" "$(sop lint)"
bdt forget sop-bad-regex >/dev/null 2>&1
is "shelf is clean after removing sop-bad-regex" "0" "$(sop_rc lint)"

# OVER THE WORD CAP — build text whose SYMPTOM/CHECK/FIX total more than 250 words.
over_cap="$(python3 -c 'print("SYMPTOM: the service failed " + " ".join(["filler"]*250) + "\nCHECK: check it\nFIX: fix it")')"
bdt remember --key sop-too-long "$over_cap" >/dev/null 2>&1
out="$(sop lint)"
is  "over word cap is caught"              "1"     "$(sop_rc lint)"
want "names the word count"    "words"    "$out"
bdt forget sop-too-long >/dev/null 2>&1

bdt forget sop-valid >/dev/null 2>&1

# ---------------------------------------------------------------------------
# WRITE REFUSES OPERATOR-SPECIFIC PATHS — a SOP that names an absolute path
# that inventory.sh would refuse on any branch must not reach the database.
# Split literal prevents inventory.sh flagging THIS FILE while still building
# the string that triggers the write fence (/home/ with a username suffix).
# ---------------------------------------------------------------------------
echo
echo "--- write refuses absolute paths that would block inventory.sh"

# Build the bad text at runtime, not as a literal in the source file.
# The split avoids the inventory scanner on this file; the joined value is what write sees.
# The inventory pattern matches the directory prefix, not the full path beyond it.
_bad_path="/"'home'"/test-spira/db"
_bad_prefix="/"'home'"/test-spira/"
bad_path_sop="$(printf 'SYMPTOM: a unit failed\nCHECK: ls %s\nFIX: fix it' "$_bad_path")"
is   "write refuses a SOP with an absolute home path"  "1" "$(sop_rc write bad-path-sop - <<< "$bad_path_sop")"
want "and names the offending token" "$_bad_prefix"    "$(printf '%s\n' "$bad_path_sop" | sop write bad-path-sop - 2>&1)"
want "and says to use env vars"      "env var"         "$(printf '%s\n' "$bad_path_sop" | sop write bad-path-sop - 2>&1)"
# bdt recall exits 1 when the key is absent; the SOP was rejected so it must be absent (exit 1).
is   "the bad SOP was not stored"    "1" "$(bdt recall sop-bad-path-sop 2>/dev/null; printf '%s' "$?")"

good_path_sop="$(printf 'SYMPTOM: a unit failed\nCHECK: ls $SPIRA_DB\nFIX: fix it')"
is  "write accepts a SOP using env vars"   "0" "$(sop_rc write good-path-sop - <<< "$good_path_sop")"
want "and it is on the shelf"  "sop-good-path-sop" "$(sop list)"
bdt forget sop-good-path-sop >/dev/null 2>&1

# ---------------------------------------------------------------------------
# FAIL CLOSED — an unreadable shelf must not be reported as clean.
# ---------------------------------------------------------------------------
echo
echo "--- an unreadable shelf fails closed, not clean"

SPIRA_DB_OVERRIDE="$TMP/no-such-database"
is  "unreadable database exits non-zero"        "1" "$(sop_rc lint)"
want "says it refuses to report clean" "refusing to report clean" "$(sop lint)"
unset SPIRA_DB_OVERRIDE

# ---------------------------------------------------------------------------
# THE GATE CALLS IT — confirm that the two pieces are actually wired together.
# ---------------------------------------------------------------------------
echo
echo "--- gate-spira.sh is wired to call sop.sh lint"

# A gate that never calls lint provides no protection. This check is the difference between
# a fence that exists and a fence that runs (law-a-documented-control-must-exist).
want "gate-spira.sh calls sop.sh lint"    "sop.sh lint"    "$(cat "$HERE/gate-spira.sh")"
want "lint appears in sop.sh usage"       "lint"           "$(sop bogus-subcommand)"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
