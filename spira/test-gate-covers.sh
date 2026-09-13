#!/usr/bin/env bash
#
# test-gate-covers.sh — the landing gate selects non-gated suites by their # covers:
# globs; changed files matched by no suite trigger the whole non-gated set as a fallback.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# gate-spira.sh runs a fixed set of suites (gate-suites) on every branch. This bead adds
# coverage-based selection: non-gated suites whose `# covers:` globs intersect the branch's
# diff are also run, so a change to spira/incident.sh triggers test-incident-dedup.sh at
# the gate rather than waiting for the next timed-run window.
#
# THE TWO-SIDED POSITIVE CONTROL (law-absence-needs-a-positive-control).
# A selector that silently never fires looks identical to one that fires and finds nothing.
# Two cases prove both directions:
#
#   A. src/a.sh changes → test-cv-fail.sh (covers src/a.sh, exits 1) is selected and
#      refused at the gate. Without selection, a non-gated suite is never reached.
#
#   B. Same failing suite, but with its covers line removed. src/a.sh is now unmapped
#      (no suite explicitly declares it). The unmapped-runs-everything fallback fires,
#      test-cv-fail.sh runs as part of the whole non-gated set, and the gate still refuses.
#      Absence of an explicit declaration must not read as a pass.
#
# covers: spira/gate-spira.sh
# shellcheck disable=SC1090
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

. "$HERE/testdb.sh"
testdb_require test-gate-covers
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up coversgate || { echo "test-gate-covers: could not build fixture database"; exit 1; }

# THE HARNESS UNDER TEST — a copy of gate-spira.sh and its fence dependencies in a temp
# directory. gate-spira.sh locates everything from its own path (HERE), so a copy with
# those files beside it runs against the fixture database and controllable suites.
SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE/gate-spira.sh" "$HERE/lib.sh" "$HERE/conf.sh" \
   "$HERE/suite-covers.sh" \
   "$HERE/exclude.sh" "$HERE/inventory.sh" "$HERE/sop.sh" \
   "$HERE/literal-lint.sh" "$HERE/orphan-test.sh" "$SH/"
[ -f "$HERE/inventory-deny" ] && cp "$HERE/inventory-deny" "$SH/"
printf 'spira | %s\n' "$TMP" > "$SH/repo-map"

# A MINIMAL GIT REPOSITORY so exclude.sh (git ls-files) and inventory.sh can run.
# inventory.sh refuses when nothing is tracked — add one harmless file.
git init -q -b main "$TMP" 2>/dev/null || true
printf 'marker\n' > "$TMP/marker.txt"
git -C "$TMP" add marker.txt 2>/dev/null || true
git -C "$TMP" -c user.email=t@t -c user.name=t commit -q -m init 2>/dev/null || true

# --------------------------------------------------------------------------------------
# THE FIXTURE SUITES. Each is a one-liner — the covers line is the only meaningful field.
#
#   test-cv-a.sh   — covers src/a.sh, always passes
#   test-cv-b.sh   — covers src/b.sh, always passes
#   test-cv-nc.sh  — no covers line (covers everything / never skipped)
#   test-cv-fail.sh — will be created with various covers lines per test case
#   test-cv-gated.sh — placed in gate-suites; the only always-gated suite
# --------------------------------------------------------------------------------------
mk_suite() {  # mk_suite <name> <covers> <exit>
    local name="$1" covers="$2" rc="$3"
    {   printf '#!/usr/bin/env bash\n'
        [ -n "$covers" ] && printf '# covers: %s\n' "$covers"
        printf 'set -uo pipefail\nexit %s\n' "$rc"
    } > "$SH/$name"; chmod +x "$SH/$name"
}
mk_suite test-cv-a.sh    "src/a.sh" 0
mk_suite test-cv-b.sh    "src/b.sh" 0
mk_suite test-cv-nc.sh   ""         0    # no covers line
mk_suite test-cv-gated.sh "gate/always.sh" 0

# gate-suites: only test-cv-gated.sh is always-gated.
GATED_LIST='spira/test-cv-gated.sh'
gate_rc=0
GOUT="$TMP/gate-out"
FLIST="$TMP/flist"

run_gate() {
    # run_gate <file-list-lines> [<extra-gate-suites-lines>]
    # Sets gate_rc and writes stdout+stderr to GOUT.
    local files="${1:-}" extra="${2:-}"
    printf '%s\n' "$GATED_LIST" > "$SH/gate-suites"
    [ -n "$extra" ] && printf '%s\n' "$extra" >> "$SH/gate-suites"
    printf '%s\n' "$files" > "$FLIST"
    (
        unset SPIRA_HOME
        cd "$TMP"
        SPIRA_CONF="/nonexistent.conf" SPIRA_DB="$SPIRA_DB" \
        SPIRA_GATE_FILES="$FLIST" \
            bash spira/gate-spira.sh 2>&1
    ) > "$GOUT"; gate_rc=$?
}

echo "test-gate-covers.sh — coverage-based suite selection"

# --------------------------------------------------------------------------------------
# 1. SELECTION: src/a.sh changes → test-cv-a.sh selected; test-cv-b.sh is not.
#
# POSITIVE CONTROL: the assertion about test-cv-a.sh would pass vacuously if selection
# simply ran everything. test-cv-b.sh NOT running is what proves it is genuinely
# selective (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
run_gate "src/a.sh"; out="$(cat "$GOUT")"
is   "selection: gate exits 0"                                       0 "$gate_rc"
want "selection: test-cv-a.sh runs for its covered file"  "test-cv-a.sh" "$out"
nowant "selection: test-cv-b.sh does not run (src/b.sh unchanged)" "test-cv-b.sh" "$out"

# --------------------------------------------------------------------------------------
# 2. NO-COVERS ALWAYS-INCLUDE: test-cv-nc.sh has no # covers: line and always runs
# alongside any non-empty selection.
# --------------------------------------------------------------------------------------
want "no-covers suite is included in any selection"       "test-cv-nc.sh" "$out"

# --------------------------------------------------------------------------------------
# 3. UNMAPPED FALLBACK: src/unmapped.sh is declared by no suite → all non-gated suites
# run. Both test-cv-a.sh (covers src/a.sh) and test-cv-b.sh (covers src/b.sh) must
# appear in the output even though neither covers the unmapped file directly.
# --------------------------------------------------------------------------------------
run_gate "src/unmapped.sh"; out="$(cat "$GOUT")"
is   "fallback: gate exits 0 (all passing)"                          0 "$gate_rc"
want "fallback: test-cv-a.sh runs (unmapped file triggers all)"  "test-cv-a.sh" "$out"
want "fallback: test-cv-b.sh runs (unmapped file triggers all)"  "test-cv-b.sh" "$out"
want "fallback: announcement names the unmapped file"          "unmapped"  "$out"

# --------------------------------------------------------------------------------------
# 4. POSITIVE CONTROL A: a failing suite for the changed file refuses the gate.
#
# test-cv-fail.sh covers src/a.sh and exits 1. Without coverage selection the gate
# would reach only test-cv-gated.sh and exit 0. The assertion that gate exits 1 is
# therefore a direct test of the selector firing. Absence of selection → passes vacuously.
# --------------------------------------------------------------------------------------
mk_suite test-cv-fail.sh "src/a.sh" 1
run_gate "src/a.sh"; out="$(cat "$GOUT")"
is   "positive-A: gate exits 1 (selected failing suite refuses it)"  1 "$gate_rc"
want "positive-A: failing suite appears in output"         "test-cv-fail.sh" "$out"

# --------------------------------------------------------------------------------------
# 5. POSITIVE CONTROL B: with the covers line removed, the changed file becomes unmapped
# (no suite explicitly declares it). The unmapped-runs-everything fallback fires and
# still runs the failing suite as part of the whole non-gated set.
#
# The changed file here is src/uniq.sh — not declared by test-cv-a.sh (src/a.sh) or
# test-cv-b.sh (src/b.sh). With test-cv-fail.sh's covers line removed, no suite
# explicitly claims src/uniq.sh → unmapped → fallback → test-cv-fail.sh runs → fails.
# --------------------------------------------------------------------------------------
mk_suite test-cv-fail.sh "" 1    # no covers line — src/uniq.sh becomes unmapped
run_gate "src/uniq.sh"; out="$(cat "$GOUT")"
is   "positive-B: gate exits 1 via unmapped fallback"                1 "$gate_rc"
want "positive-B: fallback announcement appears"               "unmapped"  "$out"

# --------------------------------------------------------------------------------------
# 6. BUDGET LINE: cost total= appears on every run regardless of selection path.
# --------------------------------------------------------------------------------------
mk_suite test-cv-fail.sh "src/a.sh" 0   # restore to passing for budget test
run_gate "src/a.sh"; out="$(cat "$GOUT")"
want "budget: cost total= line present"                      "cost total=" "$out"

# --------------------------------------------------------------------------------------
# 7. STRUCTURAL: suite_covers_of is the only # covers: parser. Neither gate-spira.sh
# nor suites.sh may carry an inline sed substitution after sp-dt8u.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): create a temp file with the
# inline sed substitution, verify the grep finds it — then verify no real file outside
# suite-covers.sh carries the same pattern.
#
# Pattern is constructed from two separate literals (_cpa + _cpb) so the full string
# 'covers: *//' does not appear as a continuous span in this file — which would cause
# grep -F to find this file and count it as an offender. -F (fixed-string) is required
# because the pattern contains '*' which grep BRE treats as a quantifier, not a literal.
# --------------------------------------------------------------------------------------
_cpa='covers:'; _cpb=' *//'
_off="$TMP/fake-parser.sh"
printf '#!/usr/bin/env bash\n_cov="$(sed '"'"'s/^# %s%s'"'"' file)"\n' "$_cpa" "$_cpb" > "$_off"
_found="$(grep -Fl "${_cpa}${_cpb}" "$_off" 2>/dev/null || true)"
is "structural positive control: grep -F finds inline parser" "$_off" "$_found"

_dup="$(grep -rFl "${_cpa}${_cpb}" "$HERE"/*.sh 2>/dev/null \
    | grep -Fv suite-covers.sh \
    | grep -Fv test-gate-covers.sh \
    || true)"
is "no duplicate covers: parser outside suite-covers.sh" "" "$_dup"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
