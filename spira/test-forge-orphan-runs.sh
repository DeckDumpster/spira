#!/usr/bin/env bash
#
# test-forge-orphan-runs.sh — forge.sh runs-for-branch and runs-queue-branches, the two
# primitives an abandoned/evicted batch's orphaned Gate run is found and cancelled through
# (sp-1p04d).
#
# runs-for-branch <repo> <branch>  → "<id> <status>" for each NON-COMPLETED Gate run on
#   that exact branch. Used at abandon/eviction time, when the branch is already known.
# runs-queue-branches <repo>       → "<id> <branch> <status>" for each NON-COMPLETED Gate
#   run whose head branch is spira/queue/*. Used by the periodic sweep, which does not
#   know in advance which branch (if any) was orphaned.
#
# Neither must ever report a completed run (nothing to cancel) or a non-queue branch
# (main's push gate, an acceptance dispatch) — those are exactly the runs sp-1p04d
# requires the sweep to leave alone.
#
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-forge-orphan-runs.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# gh stub: a --branch invocation (runs-for-branch) answers from branch-payload; a plain
# `run list` (runs-queue-branches) answers from all-payload.
cat > "$TMP/gh" <<STUB
#!/usr/bin/env bash
case "\$*" in
    *"--branch"*) cat "$TMP/branch-payload" ;;
    *)            cat "$TMP/all-payload" ;;
esac
STUB
chmod +x "$TMP/gh"

run_for_branch()     { SPIRA_GH="$TMP/gh" bash "$HERE/forge.sh" runs-for-branch "$TMP" "spira/queue/20260924T000000Z" 2>/dev/null; }
run_queue_branches()  { SPIRA_GH="$TMP/gh" bash "$HERE/forge.sh" runs-queue-branches "$TMP" 2>/dev/null; }

echo
echo "runs-for-branch: only the non-completed run on the branch is reported:"
cat > "$TMP/branch-payload" <<'J'
[
 {"databaseId": 1001, "status": "in_progress"},
 {"databaseId": 1000, "status": "completed"}
]
J
out="$(run_for_branch)"
is "one line of output" "1" "$(printf '%s\n' "$out" | grep -c .)"
is "the in-progress run is reported, with its status" "1001 in_progress" "$out"

echo
echo "runs-for-branch: every run completed → nothing to cancel:"
cat > "$TMP/branch-payload" <<'J'
[{"databaseId": 2000, "status": "completed"}]
J
is "no output" "" "$(run_for_branch)"

echo
echo "runs-queue-branches: excludes a non-queue branch (main's push gate):"
cat > "$TMP/all-payload" <<'J'
[
 {"databaseId": 3000, "headBranch": "main", "status": "in_progress"},
 {"databaseId": 3001, "headBranch": "spira/queue/20260924T010000Z", "status": "in_progress"}
]
J
out="$(run_queue_branches)"
is "one line of output (main excluded)" "1" "$(printf '%s\n' "$out" | grep -c .)"
is "only the queue-branch run is reported" \
    "3001 spira/queue/20260924T010000Z in_progress" "$out"

echo
echo "runs-queue-branches: excludes a completed run on a queue branch (nothing to cancel):"
cat > "$TMP/all-payload" <<'J'
[{"databaseId": 4000, "headBranch": "spira/queue/20260924T020000Z", "status": "completed"}]
J
is "no output" "" "$(run_queue_branches)"

echo
echo "runs-queue-branches: garbage payload yields no output, not a crash:"
printf 'not json' > "$TMP/all-payload"
is "no output on unparseable payload" "" "$(run_queue_branches)"

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
