#!/usr/bin/env bash
#
# test-suite-state-fence.sh — suite-state-fence.sh catches bad quarantine entries.
#
# POSITIVE CONTROL IN EVERY RULE: each check is proved capable of finding an offender
# before its silence is treated as evidence of a clean tree
# (law-absence-needs-a-positive-control).
#
# RULES COVERED.
# 1. Structural (via suite_state_lint): no bead for quarantine, missing suite, missing reason.
# 2. CLOSED bead: a quarantine against a CLOSED bead has no exit path — suites.sh hygiene
#    requires land_state:LANDED, which CLOSED never satisfies.
# 3. The fence is wired into gate-spira.sh (law-a-documented-control-must-exist).
#
# REAL bd AGAINST A THROWAWAY DATABASE: the CLOSED-bead check calls bd show; a stub would
# hide real serialization or field-name divergences (law-prefer-the-real-dependency).
#
# NON-DEFAULT CONFIGURED VALUES: SPIRA_CONF=/dev/null so conf.sh does not touch the
# operator's live config; SPIRA_DB is the throwaway fixture.
#
# covers: spira/suite-state-fence.sh spira/suite-state.sh spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-suite-state-fence.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-suite-state-fence
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up suite_state_fence || { echo "test-suite-state-fence: could not build fixture database"; exit 1; }

bdt() { "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

# ---------------------------------------------------------------------------
# FIXTURE LAYOUT.
#
# The fence derives STATE_FILE as suite_state_file("$HERE/..") where HERE is
# the directory containing suite-state-fence.sh. We place that in TMP/repo/spira,
# so STATE_FILE resolves to TMP/repo/spira/../spira/suite-state = TMP/repo/spira/suite-state.
# The fence also passes $HERE as the suite-dir to suite_state_lint, which checks
# that each named suite file exists there.
# ---------------------------------------------------------------------------
SPIRADIR="$TMP/repo/spira"
BINDIR="$TMP/bin"
mkdir -p "$SPIRADIR" "$BINDIR"

for f in suite-state-fence.sh suite-state.sh lib.sh conf.sh suite-covers.sh; do
    cp "$HERE/$f" "$SPIRADIR/$f"
done
# A fake suite file: used for entries that should pass the suite-existence check.
printf '#!/usr/bin/env bash\nexit 0\n' > "$SPIRADIR/test-fake-suite.sh"

STATE="$SPIRADIR/suite-state"

# bd wrapper that preserves HOME (needed so dolt config is reachable).
printf '#!/usr/bin/env bash\nHOME=%s exec %s "$@"\n' "$HOME" "$(type -P bd)" > "$BINDIR/bd"
chmod +x "$BINDIR/bd"

bdt() { HOME="$HOME" "$BINDIR/bd" -C "$SPIRA_DB" "$@"; }

# fence_out: run the fence and capture output (stdout+stderr merged).
fence_out() {
    HOME="$HOME" SPIRA_BD="$BINDIR/bd" SPIRA_DB="$SPIRA_DB" SPIRA_CONF=/dev/null \
        bash "$SPIRADIR/suite-state-fence.sh" 2>&1
}
# fence_rc: run the fence and emit only the exit code.
fence_rc() {
    HOME="$HOME" SPIRA_BD="$BINDIR/bd" SPIRA_DB="$SPIRA_DB" SPIRA_CONF=/dev/null \
        bash "$SPIRADIR/suite-state-fence.sh" >/dev/null 2>&1
    printf '%s' "$?"
}

# ---------------------------------------------------------------------------
# POSITIVE CONTROL — structural errors caught before any bead check.
# Plant each offender, verify the fence fires (SEEN RED), then withdraw and
# verify the fence passes (SEEN GREEN).
# ---------------------------------------------------------------------------
echo
echo "--- structural errors: positive controls before trusting silence"

# 1. Quarantine with no bead — suite_state_lint must catch this.
printf 'test-fake-suite.sh | quarantined | 2026-01-01T00:00:00Z | | reason here\n' > "$STATE"
out="$(fence_out)"
is  "no-bead quarantine: fence exits 1"       "1" "$(fence_rc)"
want "it reports the missing bead"            "no bead" "$out"
: > "$STATE"
is  "no-bead withdrawn: exits 0"             "0" "$(fence_rc)"

# 2. Line naming a suite that does not exist.
printf 'no-such-suite-xyz.sh | disabled | 2026-01-01T00:00:00Z | | reason\n' > "$STATE"
out="$(fence_out)"
is  "missing suite: fence exits 1"            "1" "$(fence_rc)"
want "it names the non-existent suite"        "no-such-suite-xyz.sh" "$out"
: > "$STATE"
is  "missing suite withdrawn: exits 0"       "0" "$(fence_rc)"

# 3. Missing reason.
printf 'test-fake-suite.sh | disabled | 2026-01-01T00:00:00Z | |\n' > "$STATE"
is  "missing reason: fence exits 1"           "1" "$(fence_rc)"
: > "$STATE"
is  "missing reason withdrawn: exits 0"      "0" "$(fence_rc)"

# ---------------------------------------------------------------------------
# CLOSED BEAD — the new rule.
# Plant a quarantine pointing at a CLOSED bead (SEEN RED), confirm the fence
# refuses it, then withdraw and plant a quarantine pointing at an OPEN bead
# (SEEN GREEN) to confirm the fence passes.
# ---------------------------------------------------------------------------
echo
echo "--- closed-bead quarantine: positive control then withdrawal"

closed_id="$(bdt create "test-suite-state-fence: closed bead" -l "plan" 2>/dev/null \
    | grep -oE '[a-z]+-[a-z0-9]+' | head -1)" || closed_id=""
case "${closed_id:-}" in
    ''|*[!A-Za-z0-9-]*|-*|*-) printf 'FAIL: could not create closed bead\n' >&2; exit 1 ;;
esac
bdt close "$closed_id" --reason "closed for test" >/dev/null 2>&1

printf 'test-fake-suite.sh | quarantined | 2026-01-01T00:00:00Z | %s | flaky\n' \
    "$closed_id" > "$STATE"
out="$(fence_out)"
is  "closed-bead quarantine: fence exits 1"   "1" "$(fence_rc)"
want "it names the suite"                      "test-fake-suite.sh" "$out"
want "it names the bead id"                    "$closed_id"         "$out"
want "it says CLOSED"                          "CLOSED"             "$out"

: > "$STATE"
is  "closed-bead withdrawn: exits 0"         "0" "$(fence_rc)"

open_id="$(bdt create "test-suite-state-fence: open bead" -l "plan" 2>/dev/null \
    | grep -oE '[a-z]+-[a-z0-9]+' | head -1)" || open_id=""
case "${open_id:-}" in
    ''|*[!A-Za-z0-9-]*|-*|*-) printf 'FAIL: could not create open bead\n' >&2; exit 1 ;;
esac
printf 'test-fake-suite.sh | quarantined | 2026-01-01T00:00:00Z | %s | flaky\n' \
    "$open_id" > "$STATE"
is  "open-bead quarantine: fence exits 0"     "0" "$(fence_rc)"
: > "$STATE"
bdt close "$open_id" --reason "test done" >/dev/null 2>&1

# ---------------------------------------------------------------------------
# CLEAN TREE — no entries: must exit 0 and say clean.
# ---------------------------------------------------------------------------
echo
echo "--- clean state file"
: > "$STATE"
out="$(fence_out)"
is  "empty state: exits 0"                    "0" "$(fence_rc)"
want "reports clean"                           "clean"             "$out"

# ---------------------------------------------------------------------------
# THE GATE CALLS IT (law-a-documented-control-must-exist).
# ---------------------------------------------------------------------------
echo
echo "--- gate-spira.sh is wired to call suite-state-fence.sh"
want "gate-spira.sh references the fence"     "suite-state-fence.sh" "$(cat "$HERE/gate-spira.sh")"

echo
printf 'ASSERTIONS %d\n' "$((pass+fail))"
[ "$fail" -eq 0 ]
