#!/usr/bin/env bash
#
# test-output-visibility.sh — a suite that exits non-zero must produce combined output;
# silent failure (no stdout/stderr) leaves the filed bead body empty and the failure
# undiagnosable. suites.sh detects empty output on a failure path and annotates it.
#
# WHAT THIS GUARDS. The timed runner filed beads for failing suites that had redirected
# all their output away from stdout/stderr — the bead body was empty, containing no
# context for whoever would work the bead. The runner now detects a suite that exits
# non-zero with no combined output and appends a sentinel so the bead body is never
# empty on a failure path.
#
# POSITIVE CONTROL IS FIRST (law-absence-needs-a-positive-control). Before trusting
# that the guard fires, the offender suite is run bare and confirmed to produce no
# output — establishing that the guard is needed, not already satisfied by the suite
# itself.
#
# THE INTAKE IS THE REAL ONE on a throwaway database (law-prefer-the-real-dependency).
#
# EVERY CONFIGURED VALUE IS PINNED TO A NON-DEFAULT (law-gates-run-in-a-clean-environment).
#
# defect: sp-8om6z sp-3rizz sp-z7ntu
# covers: spira/suites.sh
# timeout: 60
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-output-visibility.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-output-visibility
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up output-visibility || { echo "test-output-visibility: could not build fixture database"; exit 1; }

SH="$TMP/spira"; RUN="$TMP/run"; STATE="$TMP/state"; GATEF="$TMP/gate-suites"
mkdir -p "$SH" "$RUN" "$STATE" "$TMP/home" "$TMP/repo"
cp "$HERE/suites.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/incident.sh" "$HERE/incident-dedup-decision.py" "$SH/"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$TMP/ask.log" > "$SH/ask.sh"
chmod +x "$SH/ask.sh"

# Knobs, all pinned away from shipped defaults.
BUDGET=60; PERSUITE=20; STALE=3600; PRIO=3; REPONAME=output-visibility-fixture

# Repo-map so incident.sh can validate the repo: label.
printf '%s | %s | push | main | : | :\n' "$REPONAME" "$TMP/repo" > "$SH/repo-map"

# TOOLPATH: bd wrapper that brings HOME into the hermetic env for bd's dolt config.
# conf.sh replaces PATH outright; the bd wrapper must arrive via SPIRA_PATH so the
# rebuilt PATH still resolves bd for incident.sh.
TOOLPATH="$TMP/bin"; mkdir -p "$TOOLPATH"
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$TOOLPATH/bd"
chmod +x "$TOOLPATH/bd"

sut() {
    local cmd="$1"; shift
    # SPIRA_SUITES_INLINE=1 — run the planted suites HERE, not in a container.
    # suites.sh delegates its pass to testenv-batch.sh (law-tests-run-only-through-testenv-
    # batch). This suite drives suites.sh's OWN logic against fake suites it planted in a
    # scratch tree, so a container would have to be started per invocation to run code that
    # exists only to be counted. Containment is not being waived: this suite is itself run
    # inside a container by the timed pass, so the planted suites are already contained by it.
    env -i SPIRA_SUITES_INLINE=1 PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF="$TMP/no-such.conf" \
        SPIRA_HOME="$SH" SPIRA_REPO="$TMP/repo" SPIRA_HOME_REPO="$REPONAME" \
        SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SUITES_STATE="$STATE" SPIRA_GATE_SUITES="$GATEF" \
        SPIRA_SUITES_BUDGET="$BUDGET" SPIRA_SUITE_TIMEOUT="$PERSUITE" \
        SPIRA_SUITES_STALE="$STALE" SPIRA_SUITES_PRIORITY="$PRIO" \
        SPIRA_NOTIFY="$SH/ask.sh" \
        SPIRA_PATH="$TOOLPATH" \
        SPIRA_SUITES_RUNNER_VARS="" \
        SPIRA_INCIDENT_LOCK_WAIT="60" \
        "$@" bash "$SH/suites.sh" "$cmd" 2>&1
}
plant() { cat > "$SH/$1"; chmod +x "$SH/$1"; }
B() { bd -C "$SPIRA_DB" "$@"; }
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
' "$1"
}
count() { printf '%s\n' "$1" | grep -c . || true; }

# Gate file is empty: all planted suites go to the timed pass.
printf '# nothing in the gate for this fixture\n' > "$GATEF"

# ======================================================================================
echo
echo "positive control — the offender suite produces no output when run bare:"
# ======================================================================================
# THE OFFENDER: redirects all output away before any assertion can print. This is the
# exact shape that produced undiagnosable beads — the runner sees an empty $out, the
# filed bead body contains nothing after "--- output ---", and nobody can tell what failed.
plant test-fx-silent.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
exec >/dev/null 2>&1
echo "  FAIL  this line would explain the failure but nobody sees it"
exit 1
S

bare_out="$(bash "$SH/test-fx-silent.sh" 2>&1)"
is "positive control: offender produces no output when run bare" "" "$bare_out"

# ======================================================================================
echo
echo "verbose suite: exits non-zero and produces FAIL output (the good baseline):"
# ======================================================================================
plant test-fx-verbose.sh <<'S'
#!/usr/bin/env bash
# covers: spira/suites.sh
echo "  FAIL  verbose-failure-marker: expected [x] got [y]"
exit 1
S

# ======================================================================================
echo
echo "after sut run: silent suite bead has the no-output diagnostic; verbose bead has the FAIL line:"
# ======================================================================================
run_out="$(sut run)"

# Runner must report both suites as RED.
want "runner output reports silent suite" "test-fx-silent" "$run_out"
want "runner output reports verbose suite" "test-fx-verbose" "$run_out"

# A bead must be filed for each suite.
silent_beads="$(beads_titled 'test-fx-silent')"
is "a bead is filed for the silent suite" "1" "$(count "$silent_beads")"

verbose_beads="$(beads_titled 'test-fx-verbose')"
is "a bead is filed for the verbose suite" "1" "$(count "$verbose_beads")"

# THE DIAGNOSTIC MUST APPEAR IN THE SILENT SUITE'S BEAD BODY. Without the guard in
# suites.sh, the output section would be empty and nobody could diagnose the failure.
silent_id="$(printf '%s\n' "$silent_beads" | head -1 | tr -d '[:space:]')"
if [ -n "$silent_id" ]; then
    silent_body="$(B show "$silent_id" 2>/dev/null)"
    want "silent suite bead body contains the no-output diagnostic" "no output" "$silent_body"
    nowant "silent suite bead body does not contain the suppressed FAIL line" "this line would explain" "$silent_body"
else
    bad "silent suite bead id" "not found — cannot check bead body"
fi

# THE VERBOSE SUITE'S FAIL LINE MUST APPEAR IN ITS BEAD BODY. The guard must not
# overwrite real output — only annotate when output is actually empty.
verbose_id="$(printf '%s\n' "$verbose_beads" | head -1 | tr -d '[:space:]')"
if [ -n "$verbose_id" ]; then
    verbose_body="$(B show "$verbose_id" 2>/dev/null)"
    want "verbose suite bead body contains the FAIL line" "verbose-failure-marker" "$verbose_body"
    nowant "verbose suite bead body does not contain the no-output diagnostic" "no output" "$verbose_body"
else
    bad "verbose suite bead id" "not found — cannot check bead body"
fi

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
