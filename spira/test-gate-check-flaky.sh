#!/usr/bin/env bash
#
# test-gate-check-flaky.sh — gate-check.sh files P2 beads for flaky suite annotations,
# deduplicated on the suite name while one is open.
#
#   ./test-gate-check-flaky.sh
#
# THREE THINGS VERIFIED.
#
# 1. POSITIVE CONTROL: a completed run with two "flaky suite" annotations files two beads.
#
# 2. DEDUP: a second pass over the same run files none; the open beads suppress filing.
#
# 3. NO-OP: a run with no "flaky suite" annotations files nothing.
#
# A STUB gh FOR ALL PARTS. What is tested is gate-check.sh's annotation parsing and bead
# filing; a real GitHub API response is unreachable in CI and would test the wrong layer.
#
# A REAL bd AGAINST A THROWAWAY DATABASE (law-prefer-the-real-dependency). Bead filing
# goes through bead.sh -> bdq, so a stub would hide real filing failures.
#
# No bd wrapper needed: the testdb has no gates, so `bd gate check` is a silent no-op.
#
# covers: spira/gate-check.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "want [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-gate-check-flaky
trap 'testdb_drop' EXIT INT TERM
testdb_up gate_check_flaky || { echo "test-gate-check-flaky: could not build fixture"; exit 1; }

B() { "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/sbin" "$TMP/run" "$TMP/run/events"

# bead.sh refuses a repo: label absent from the map (spira/test-bead-repo-guard.sh); the
# home repo is $(basename "$TMP") here, so it must have its own row like any other.
printf '%s | %s | push | origin/main |  |\n' "$(basename "$TMP")" "$TMP" > "$TMP/repo-map"

# Count beads whose title starts with "flaky suite:".
count_flaky() {
    B list --json 2>/dev/null | python3 -c '
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    print(sum(1 for b in beads if (b.get("title") or "").startswith("flaky suite:")))
except Exception:
    print(0)
' 2>/dev/null
}

run_gate_check() {
    # SPIRA_PATH rather than PATH: conf.sh overwrites PATH entirely; this puts $TMP/sbin
    # at the front of the path it builds so command -v gh finds the stub.
    SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$TMP/sbin" \
        SPIRA_REPO_MAP="$TMP/repo-map" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_FLAKY_GH_REPO="test-org/test-repo" \
        bash "$HERE/gate-check.sh" 2>/dev/null
}

# --------------------------------------------------------------------------------------
# PART 1: POSITIVE CONTROL — two "flaky suite" annotations → two beads filed.
#
# Run 42 has two jobs; each job has one flaky suite annotation. Gate check must file
# two beads, one per suite, through bead.sh.
# --------------------------------------------------------------------------------------
echo "two flaky annotations → two beads:"

testdb_reset

cat > "$TMP/sbin/gh" <<'GHSTUB'
#!/usr/bin/env bash
case "$*" in
    *"run list"*)
        printf '42\n' ;;
    *"actions/runs/42/jobs"*)
        printf '{"jobs":[{"id":101},{"id":102}]}\n' ;;
    *"check-runs/101/annotations"*)
        printf '[{"annotation_level":"warning","title":"flaky suite","message":"test-foo.sh was red, then green on a serial re-run"}]\n' ;;
    *"check-runs/102/annotations"*)
        printf '[{"annotation_level":"warning","title":"flaky suite","message":"test-bar.sh was red, then green on a serial re-run"}]\n' ;;
    *) exit 0 ;;
esac
GHSTUB
chmod +x "$TMP/sbin/gh"

run_gate_check

is "two flaky beads are filed"      "2" "$(count_flaky)"

foo_count="$(B list --json 2>/dev/null | python3 -c '
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    print(sum(1 for b in beads if b.get("title") == "flaky suite: test-foo.sh"))
except Exception:
    print(0)
' 2>/dev/null)"
is "bead for test-foo.sh is filed"  "1" "$foo_count"

bar_count="$(B list --json 2>/dev/null | python3 -c '
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    print(sum(1 for b in beads if b.get("title") == "flaky suite: test-bar.sh")  )
except Exception:
    print(0)
' 2>/dev/null)"
is "bead for test-bar.sh is filed"  "1" "$bar_count"

# --------------------------------------------------------------------------------------
# PART 2: DEDUP — second pass over the same run files none; open beads suppress filing.
# --------------------------------------------------------------------------------------
echo
echo "second pass over the same run → no new beads:"

# Same gh stub, same database (beads from part 1 are still open).
run_gate_check

is "still two beads, none added"    "2" "$(count_flaky)"

# --------------------------------------------------------------------------------------
# PART 3: NO-OP — a run with no "flaky suite" annotations files nothing.
# --------------------------------------------------------------------------------------
echo
echo "run with no flaky annotations → no beads:"

testdb_reset

cat > "$TMP/sbin/gh" <<'GHSTUB'
#!/usr/bin/env bash
case "$*" in
    *"run list"*)
        printf '99\n' ;;
    *"actions/runs/99/jobs"*)
        printf '{"jobs":[{"id":201}]}\n' ;;
    *"check-runs/201/annotations"*)
        printf '[{"annotation_level":"notice","title":"some info","message":"nothing flaky here"}]\n' ;;
    *) exit 0 ;;
esac
GHSTUB
chmod +x "$TMP/sbin/gh"

run_gate_check

is "no flaky beads when run is clean" "0" "$(count_flaky)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
