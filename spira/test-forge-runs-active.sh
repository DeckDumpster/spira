#!/usr/bin/env bash
#
# test-forge-runs-active.sh — forge.sh runs-active counts only pull-request runs in flight.
#
# THE PROPERTY UNDER TEST. batch.sh cuts a batch at once when runs-active prints 0. The first
# version counted every queued or in-progress run in the repository, so a main-branch gate or
# a dispatched acceptance run held certified beads for the whole batch wait with no PR open
# anywhere (2026-09-23). Those runs do not compete with a batch: runners are cloned per run.
#
# CASES
#   push-only   a push gate and a workflow_dispatch in progress, no PR runs  → 0
#   pr-active   the same plus one in-progress pull_request run                → 1
#   pr-done     a completed pull_request run only                             → 0
#   garbage     the API returns something unparseable                         → ?  (never 0)
#   api-fail    the API call itself fails                                     → ?  (never 0)
#
# WITHOUT THE FIX push-only counts the in-progress push and the queued dispatch: 2, not 0.
#
# covers: spira/forge.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

echo "test-forge-runs-active.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# A gh stub that prints whatever $TMP/payload holds, or fails when $TMP/fail exists.
cat > "$TMP/gh" <<STUB
#!/usr/bin/env bash
[ -e "$TMP/fail" ] && exit 1
cat "$TMP/payload"
STUB
chmod +x "$TMP/gh"

run() { SPIRA_GH="$TMP/gh" bash "$HERE/forge.sh" runs-active "$TMP" 2>/dev/null; }

cat > "$TMP/payload" <<'J'
{"workflow_runs":[
 {"event":"push","status":"in_progress"},
 {"event":"workflow_dispatch","status":"queued"},
 {"event":"push","status":"completed"}]}
J
is "push-only: main gates and dispatches are not busy" "0" "$(run)"

cat > "$TMP/payload" <<'J'
{"workflow_runs":[
 {"event":"push","status":"in_progress"},
 {"event":"workflow_dispatch","status":"queued"},
 {"event":"pull_request","status":"in_progress"}]}
J
is "pr-active: an in-flight PR run is busy" "1" "$(run)"

cat > "$TMP/payload" <<'J'
{"workflow_runs":[{"event":"pull_request","status":"completed"}]}
J
is "pr-done: a finished PR run is not busy" "0" "$(run)"

printf 'not json' > "$TMP/payload"
is "garbage: unparseable payload is unknown, never 0" "?" "$(run)"

touch "$TMP/fail"
is "api-fail: a failed call is unknown, never 0" "?" "$(run)"

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
