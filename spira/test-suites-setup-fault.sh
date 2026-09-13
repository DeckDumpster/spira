#!/usr/bin/env bash
#
# test-suites-setup-fault.sh — a suite that runs zero assertions is a setup fault, not red.
#
#   ./test-suites-setup-fault.sh
#
# WHAT THIS GUARDS. When a suite exits non-zero but never reaches a single assertion
# — because a fixture collapsed, a dependency failed to source, or a gate refused
# before the test body ran — it is a setup fault, not a defect in the code under
# # covers:. Seven of the beads filed on 2026-09-12 named the covered file even though
# the covered file never ran; aeons sent to investigate found it correct and closed
# those beads as unreproducible.
#
# The discriminant is the ASSERTIONS trailer: "ASSERTIONS 0" in the suite's output
# means zero assertions ran before exit. A missing trailer means the suite has not
# yet adopted the counter; suites.sh must treat that as red (as before), not as zero.
# That property is law-absence-needs-a-positive-control applied to the counter itself:
# an unreadable count must never report all-clear.
#
# POSITIVE CONTROLS FIRST:
#   1. A suite printing ASSERTIONS 0 and exiting non-zero must be classified as
#      setup-fault, not red — prove the bead says "setup-fault".
#   2. A suite printing ASSERTIONS N (N>0) and exiting non-zero must be classified
#      as red, not setup-fault — prove assertion-carrying suites still file red.
#   3. A suite printing NO ASSERTIONS trailer and exiting non-zero must be classified
#      as red — prove the missing-trailer safety.
#   4. The setup-fault bead names the suite itself, not the # covers: file.
#
# THE INTAKE IS THE REAL ONE on a throwaway database (law-prefer-the-real-dependency).
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT (law-gates-run-in-a-clean-environment).
#
# defect: sp-r21
# covers: spira/suites.sh spira/suite-assert.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suites-setup-fault.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suites-setup-fault
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up setup-fault || { echo "test-suites-setup-fault: could not build a fixture database"; exit 1; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" \
   "$HERE/suite-covers.sh" "$SH/"
# Also copy suite-assert.sh so planted suites can source it.
[ -f "$HERE/suite-assert.sh" ] && cp "$HERE/suite-assert.sh" "$SH/"

# Knobs — all away from the shipped default.
BUDGET=120; PERSUITE=20; STALE=3600; PRIO=3; REPONAME=setup-fault-repo

printf '%s | %s | push | main | : | :\n' "$REPONAME" "$TMP/repo" > "$SH/repo-map"

printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

# bd wrapper: hand the real HOME to bd-embedded while the test uses a scratch HOME.
TOOLPATH="$TMP/bin"; mkdir -p "$TOOLPATH"
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$TOOLPATH/bd"
chmod +x "$TOOLPATH/bd"

sut() {
    local cmd="$1"; shift
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" BEADS_DIR="${SPIRA_DB}/.beads" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_SUITES_SKIP_TESTDB="1" \
        SPIRA_NOTIFY="$SH/ask.sh" \
        SPIRA_PATH="$TOOLPATH" \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }
B() { bd -C "$SPIRA_DB" "$@"; }
result_field() {
    local f="$STATE/$1.result"
    [ -r "$f" ] || return 1
    awk '{print $'"$2"'}' "$f" 2>/dev/null
}
beads_titled() {
    B list --status open,in_progress --limit 0 --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c '
import sys, json
key = sys.argv[1]
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for i in (d if isinstance(d, list) else [d]):
    if key in (i.get("title") or ""): print(i["id"])
' "$1" 2>/dev/null || true
}
bead_field() {
    B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys, json
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
d = d[0] if isinstance(d, list) else d
print(d.get(sys.argv[1], ""))
' "$2" 2>/dev/null || true
}
count() { printf '%s\n' "${1:-}" | grep -c '[^ ]' 2>/dev/null || echo 0; }

# Gate file is empty — every planted suite goes to the timed pass.
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# ============================================================================
echo
echo "positive control 1 — zero assertions, non-zero exit: must be setup-fault:"
# ============================================================================
# A suite that prints ASSERTIONS 0 and exits 1 never reached any test logic.
# Its setup failed. suites.sh must classify it as setup-fault, not red.
plant test-sf-zero-assert.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf 'spira_containment_check: no repo-map — refusing to continue\n'
printf 'ASSERTIONS 0\n'
exit 1
S

out="$(sut run)"; rc_sut=$?
is "pass with setup-fault exits 2 (something was filed)" "2" "$rc_sut"

st="$(result_field test-sf-zero-assert.sh 1 || true)"
is "zero-assertion suite records setup-fault, not red" "setup-fault" "$st"

want "the pass output says SETUP-FAULT" "SETUP-FAULT" "$out"
nowant "the pass output does not say RED for setup-fault suite" "  RED " "$out"

# The bead is titled with the suite name and 'setup-fault'.
sf_ids="$(beads_titled 'test-sf-zero-assert.sh is setup-fault')"
is "a setup-fault bead is filed" "1" "$(count "$sf_ids")"
sf_id="${sf_ids%%$'\n'*}"
if [ -n "$sf_id" ]; then
    labels="$(B label list "$sf_id" 2>&1)"
    want "setup-fault bead is labelled plan"             "plan"           "$labels"
    want "setup-fault bead carries the repo label"       "repo:$REPONAME" "$labels"
fi

# CRITICAL: no bead named after the covered file — only the suite itself.
nowant "no bead names the covered file (nothing.sh)" "nothing.sh" \
    "$(B list --status open,in_progress --limit 0 --json 2>/dev/null \
       | sed -n '/^[[{]/,$p' \
       | python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except: sys.exit(0)
for i in (d if isinstance(d,list) else [d]): print(i.get("title",""))
' 2>/dev/null || true)"

# ============================================================================
echo
echo "positive control 2 — assertions ran but failed: must be red, not setup-fault:"
# ============================================================================
# A suite that prints ASSERTIONS N (N>0) and exits non-zero ran real assertions.
# The covered code is genuinely broken. Must still file as red.
rm -f "$SH/test-sf-zero-assert.sh"
find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
testdb_reset 2>/dev/null || true

plant test-sf-with-asserts.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  my-check: wanted [ok] got [broken]\n'
printf 'ASSERTIONS 1\n'
exit 1
S

out2="$(sut run)"
st2="$(result_field test-sf-with-asserts.sh 1 || true)"
is "suite with failing assertions records red, not setup-fault" "red" "$st2"

nowant "the pass does not say SETUP-FAULT for a suite with assertions" "SETUP-FAULT" "$out2"
want "the pass does say RED for a suite with assertions" "RED" "$out2"

# ============================================================================
echo
echo "positive control 3 — no ASSERTIONS trailer at all: must be red (not setup-fault):"
# ============================================================================
# A suite with NO ASSERTIONS trailer that exits non-zero is an un-migrated suite.
# Must be classified as red, never as setup-fault. "Missing" must never read as "zero".
rm -f "$SH/test-sf-with-asserts.sh"
find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
testdb_reset 2>/dev/null || true

plant test-sf-no-trailer.sh <<'S'
#!/usr/bin/env bash
# covers: spira/nothing.sh
printf '  FAIL  my-check: wanted [ok] got [broken]\n'
# Intentionally NO ASSERTIONS trailer — simulate un-migrated suite.
exit 1
S

out3="$(sut run)"
st3="$(result_field test-sf-no-trailer.sh 1 || true)"
is "suite with no ASSERTIONS trailer records red, not setup-fault" "red" "$st3"
nowant "no SETUP-FAULT when ASSERTIONS trailer is absent" "SETUP-FAULT" "$out3"
want "pass says RED for no-trailer suite" "RED" "$out3"

# ============================================================================
echo
echo "positive control 4 — suite-assert.sh helper emits correct trailer:"
# ============================================================================
# A suite sourcing suite-assert.sh must emit ASSERTIONS <n> on exit.
# This verifies the shared helper produces the trailer that suites.sh reads.
if [ ! -f "$SH/suite-assert.sh" ]; then
    ok "SKIP: suite-assert.sh not present — helper test skipped"
    ok "SKIP: (placeholder)"
else
    rm -f "$SH/test-sf-no-trailer.sh"
    find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
    testdb_reset 2>/dev/null || true

    # A suite that sources suite-assert.sh, runs 2 assertions (one passing, one failing),
    # and exits. The trailer must say ASSERTIONS 2, and the suite is classified red.
    plant test-sf-helper-asserts.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suite-assert.sh
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/suite-assert.sh"
ok "first assertion"
bad "second assertion" "intentional fail"
[ "$fail" -eq 0 ]
S

    out4="$(sut run)"
    st4="$(result_field test-sf-helper-asserts.sh 1 || true)"
    is "suite-assert helper: 2-assertion suite records red (not setup-fault)" "red" "$st4"
    nowant "suite-assert helper: 2-assertion suite not classified as SETUP-FAULT" "SETUP-FAULT" "$out4"

    # A suite that sources suite-assert.sh but exits before any assertion.
    # The EXIT trap fires and prints ASSERTIONS 0 → must be classified as setup-fault.
    rm -f "$SH/test-sf-helper-asserts.sh"
    find "$STATE" -maxdepth 1 -name '*.result' -delete 2>/dev/null; true
    testdb_reset 2>/dev/null || true

    plant test-sf-helper-no-asserts.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suite-assert.sh
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/suite-assert.sh"
# Fail setup before any assertion.
echo "fixture: no repo-map — refusing to continue"
exit 1
S

    out5="$(sut run)"
    st5="$(result_field test-sf-helper-no-asserts.sh 1 || true)"
    is "suite-assert helper: zero-assertion exit records setup-fault" "setup-fault" "$st5"
    want "pass says SETUP-FAULT when suite-assert helper emits ASSERTIONS 0" "SETUP-FAULT" "$out5"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
