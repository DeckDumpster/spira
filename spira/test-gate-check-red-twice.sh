#!/usr/bin/env bash
#
# test-gate-check-red-twice.sh — gate-check.sh files P1 beads for suites red twice on main,
# deduplicates while a bead is open, and raises a lower-priority bead to P1.
#
#   ./test-gate-check-red-twice.sh
#
# FOUR THINGS VERIFIED.
#
# 1. POSITIVE CONTROL: a failed main run with two "red-twice suite" annotations files two P1 beads.
#
# 2. DEDUP: a second pass over the same run files none; the open beads suppress filing.
#
# 3. RAISE: a pre-existing open P3 bead for one suite is raised to P1, not duplicated.
#
# 4. NO-OP: a run whose only annotations are "flaky suite" (not "red-twice suite") files no P1 bead.
#
# A STUB gh FOR ALL PARTS. Annotation parsing and bead filing are what is tested;
# a real GitHub API response is unreachable in CI and would test the wrong layer.
#
# A REAL bd AGAINST A THROWAWAY DATABASE (law-prefer-the-real-dependency).
#
# tier: T1
# covers: spira/gate-check.sh spira/gate-retry.sh
# priority: 1
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-gate-check-red-twice
trap 'testdb_drop' EXIT INT TERM
testdb_up gate_check_red_twice || { echo "test-gate-check-red-twice: could not build fixture"; exit 1; }

B() { "${SPIRA_BD:-bd}" -C "$SPIRA_DB" "$@"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/sbin" "$TMP/run" "$TMP/run/events"

# bead.sh refuses a repo: label absent from the map (spira/test-bead-repo-guard.sh); the
# home repo is $(basename "$TMP") here, so it must have its own row like any other.
printf '%s | %s | push | origin/main |  |\n' "$(basename "$TMP")" "$TMP" > "$TMP/repo-map"

count_red_twice() {
    B list --json 2>/dev/null | python3 -c '
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    print(sum(1 for b in beads if (b.get("title") or "").startswith("suite red on main:")))
except Exception:
    print(0)
' 2>/dev/null
}

priority_of_bead() {  # priority_of_bead <title>
    B list --json 2>/dev/null | python3 -c "
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    for b in beads:
        if b.get('title') == sys.argv[1] and b.get('status') in ('open', 'in_progress'):
            print(b.get('priority', '?'))
            sys.exit(0)
    print('not found')
except Exception:
    print('error')
" "$1" 2>/dev/null
}

run_gate_check() {
    SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" SPIRA_RUN="$TMP/run" SPIRA_DB="$SPIRA_DB" \
        SPIRA_BD="$SPIRA_BD" \
        SPIRA_PATH="$TMP/sbin" \
        SPIRA_REPO_MAP="$TMP/repo-map" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_FLAKY_GH_REPO="test-org/test-repo" \
        bash "$HERE/gate-check.sh" 2>/dev/null
}

# --------------------------------------------------------------------------------------
# PART 1: POSITIVE CONTROL — two "red-twice suite" annotations in a failed main run
# → two P1 beads filed.
#
# Run 55 (failed, main branch) has two jobs; each job has one red-twice annotation.
# --------------------------------------------------------------------------------------
echo "two red-twice annotations → two P1 beads:"

testdb_reset

cat > "$TMP/sbin/gh" <<'GHSTUB'
#!/usr/bin/env bash
case "$*" in
    *"run list"*"--status success"*)
        printf '[{"headSha":"aabbcc"}]\n' ;;
    *"run list"*"--status failure"*)
        printf '55\taabbdd\n' ;;
    *"actions/runs/55/jobs"*)
        printf '{"jobs":[{"id":201},{"id":202}]}\n' ;;
    *"check-runs/201/annotations"*)
        printf '[{"annotation_level":"failure","title":"red-twice suite","message":"test-alpha.sh"}]\n' ;;
    *"check-runs/202/annotations"*)
        printf '[{"annotation_level":"failure","title":"red-twice suite","message":"test-beta.sh"}]\n' ;;
    *"run view"*"--log-failed"*)
        printf 'Suites\t2024-01-01T00:00:00.000Z\tFAIL: assertion failed\n' ;;
    *) exit 0 ;;
esac
GHSTUB
chmod +x "$TMP/sbin/gh"

run_gate_check

is "two P1 beads are filed"          "2" "$(count_red_twice)"
is "bead for test-alpha.sh is P1"    "1" "$(priority_of_bead 'suite red on main: test-alpha.sh')"
is "bead for test-beta.sh is P1"     "1" "$(priority_of_bead 'suite red on main: test-beta.sh')"

# --------------------------------------------------------------------------------------
# PART 2: DEDUP — second pass over the same run files none; open P1 beads suppress filing.
# --------------------------------------------------------------------------------------
echo
echo "second pass → no new beads:"

run_gate_check

is "still two beads, none added"     "2" "$(count_red_twice)"

# --------------------------------------------------------------------------------------
# PART 3: RAISE — a pre-existing open P3 bead for one suite is raised to P1, not duplicated.
# --------------------------------------------------------------------------------------
echo
echo "pre-existing P3 bead → raised to P1, not duplicated:"

testdb_reset

# File a P3 bead for test-alpha.sh before the gate check runs.
B create "suite red on main: test-alpha.sh" -p 3 -l "plan,repo:spira" >/dev/null 2>&1

run_gate_check

is "bead for test-alpha.sh raised to P1" "1" "$(priority_of_bead 'suite red on main: test-alpha.sh')"
is "still only one bead for test-alpha"  "1" "$(B list --json 2>/dev/null | python3 -c '
import json, sys
try:
    beads = json.load(sys.stdin)
    if not isinstance(beads, list): beads = []
    print(sum(1 for b in beads if b.get("title") == "suite red on main: test-alpha.sh"))
except Exception:
    print(0)
' 2>/dev/null)"

# --------------------------------------------------------------------------------------
# PART 4: NO-OP — a run with only "flaky suite" annotations (not "red-twice suite") files no P1.
# --------------------------------------------------------------------------------------
echo
echo "flaky-only run → no P1 red-twice beads:"

testdb_reset

cat > "$TMP/sbin/gh" <<'GHSTUB'
#!/usr/bin/env bash
case "$*" in
    *"run list"*"--status success"*)
        printf '[{"headSha":"aabbcc"}]\n' ;;
    *"run list"*"--status failure"*)
        printf '77\taabbee\n' ;;
    *"actions/runs/77/jobs"*)
        printf '{"jobs":[{"id":301}]}\n' ;;
    *"check-runs/301/annotations"*)
        printf '[{"annotation_level":"warning","title":"flaky suite","message":"test-gamma.sh was red, then green on a serial re-run"}]\n' ;;
    *) exit 0 ;;
esac
GHSTUB
chmod +x "$TMP/sbin/gh"

run_gate_check

is "no red-twice beads for flaky-only run" "0" "$(count_red_twice)"
tl_summary
