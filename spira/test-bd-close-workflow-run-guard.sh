#!/usr/bin/env bash
#
# test-bd-close-workflow-run-guard.sh — bd-close-workflow-run-guard.sh blocks
#   bd close on a branch that touches .github/workflows/ unless the close reason
#   cites a valid GitHub Actions run URL (law-a-workflow-lands-on-its-own-run).
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): prove the guard
# fires when a workflow file changed and no URL is in the close reason. If that
# does not block, every "guard allows" case that follows is vacuous.
#
# WHAT IS VERIFIED
#  1. POSITIVE CONTROL: workflow file changed, no URL → guard blocks (exits 2)
#  2. Failing-open baseline: same scenario without the guard → exits 0
#  3. Workflow changed, URL with wrong head_branch → guard blocks
#  4. Workflow changed, URL with SHA not ancestor of HEAD → guard blocks
#  5. Workflow changed, URL with wrong workflow file → guard blocks
#  6. Workflow changed, run URL not found (404) → guard blocks
#  7. Workflow changed, valid URL (right branch, SHA is ancestor, right file) → guard allows
#  8. SPIRA_WORKFLOW_ONLY_PATHS script changed, no URL → guard blocks
#  9. Only non-workflow file changed (spira/lib.sh) → guard allows (fence does not fire)
# 10. SPIRA_WORKFLOW_RUN_CONSIDERED env override → guard allows
# 11. Inline SPIRA_WORKFLOW_RUN_CONSIDERED in command → guard allows
# 12. Non-aeon session (no SPIRA_AEON) → guard allows
# 13. Non-Bash tool call → guard allows
# 14. Non-close Bash command → guard allows
#
# The API calls are intercepted via SPIRA_GH_API pointing at a stub HTTP server
# started by this suite (law-probe-a-fixture-not-production).
#
# covers: spira/bd-close-workflow-run-guard.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1"; esac; }
wantrc() { if [ "$3" -eq "$2" ]; then ok "$1"; else bad "$1" "wanted rc=$2, got rc=$3"; fi; }

echo "test-bd-close-workflow-run-guard.sh"

GUARD="$HERE/bd-close-workflow-run-guard.sh"
[ -f "$GUARD" ] || { printf 'SKIP bd-close-workflow-run-guard.sh not found\n'; exit 77; }

. "$HERE/testdb.sh"
testdb_require wf-run-guard || exit 77
TMP="$(mktemp -d)"
trap 'testdb_drop 2>/dev/null; stop_stub; rm -rf "$TMP"' EXIT INT TERM
testdb_up wf-run-guard || exit 1
BD="${SPIRA_BD:-bd}"

# ---- stub HTTP server -------------------------------------------------------
# Serves JSON from files in $TMP/stub/. Path: /repos/<owner>/<repo>/actions/runs/<id>
STUB_PORT=""
STUB_PID=""
stop_stub() { [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true; }

start_stub() {
    mkdir -p "$TMP/stub"
    python3 -c '
import http.server, json, os, sys

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        stub_dir = os.environ["STUB_DIR"]
        key = self.path.lstrip("/").replace("/", "_")
        f = os.path.join(stub_dir, key + ".json")
        if os.path.exists(f):
            data = open(f).read().encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(data)
        else:
            self.send_response(404)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{\"message\":\"Not Found\"}")

port = int(sys.argv[1])
srv = http.server.HTTPServer(("127.0.0.1", port), H)
srv.serve_forever()
' "$1" &
    STUB_PID=$!
    sleep 0.3
}

# Find a free port
STUB_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')"
STUB_DIR="$TMP/stub"
STUB_DIR="$STUB_DIR" start_stub "$STUB_PORT"
GH_API="http://127.0.0.1:$STUB_PORT"

# Set up a stub run JSON helper
stub_run() {
    # stub_run <owner> <repo> <run_id> <json>
    local key="${1}_${2}_actions_runs_${3}"
    printf '%s' "$4" > "$STUB_DIR/repos_${key}.json"
}

# Set up a git repo simulating a bead worktree
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email "test@spira.local"
git -C "$REPO" config user.name "Test"
# Base commit (simulates origin/main)
mkdir -p "$REPO/.github/workflows" "$REPO/spira"
printf 'name: Gate\n' > "$REPO/.github/workflows/gate.yml"
printf '# lib\n' > "$REPO/spira/lib.sh"
git -C "$REPO" add .
git -C "$REPO" commit -q -m "base"
BASE_SHA="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" branch -M main
# Create origin/main tracking ref so diff --name-only origin/main...HEAD works
git -C "$REPO" update-ref refs/remotes/origin/main "$BASE_SHA"

HEAD_SHA=""

# Helpers to set up branch state
branch_with_workflow() {
    git -C "$REPO" checkout -q -b test-wf-branch 2>/dev/null || git -C "$REPO" checkout -q test-wf-branch
    printf 'name: Acceptance\n# changed\n' > "$REPO/.github/workflows/acceptance.yml"
    git -C "$REPO" add .github/workflows/acceptance.yml
    git -C "$REPO" commit -q -m "add acceptance workflow"
    HEAD_SHA="$(git -C "$REPO" rev-parse HEAD)"
}

branch_with_workflow_only() {
    git -C "$REPO" checkout -q -b test-only-branch 2>/dev/null || git -C "$REPO" checkout -q test-only-branch
    printf '# acceptance-ci\n' > "$REPO/spira/acceptance-ci.sh"
    git -C "$REPO" add spira/acceptance-ci.sh
    git -C "$REPO" commit -q -m "update acceptance-ci"
    HEAD_SHA="$(git -C "$REPO" rev-parse HEAD)"
}

branch_with_lib() {
    git -C "$REPO" checkout -q -b test-lib-branch 2>/dev/null || git -C "$REPO" checkout -q test-lib-branch
    printf '# lib updated\n' > "$REPO/spira/lib.sh"
    git -C "$REPO" add spira/lib.sh
    git -C "$REPO" commit -q -m "update lib"
    HEAD_SHA="$(git -C "$REPO" rev-parse HEAD)"
}

reset_to_base() {
    git -C "$REPO" checkout -q main 2>/dev/null
    git -C "$REPO" branch -D test-wf-branch test-only-branch test-lib-branch 2>/dev/null || true
}

# Create a test bead and set it in_progress
BID="$("$BD" -C "$SPIRA_DB" create --title "test-workflow-guard" -l spira --type task \
        2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID" ] || { printf 'SKIP could not create test bead\n'; exit 77; }
ACTOR="aeon-test-wf"
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID" --claim 2>/dev/null

CLOSE_CMD="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done without URL
REASON"

make_payload() {
    python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1]}})
)' "$1"
}

run_guard() {
    local rc out
    out=$(make_payload "$CLOSE_CMD" | \
      env -i PATH="$PATH" HOME="$HOME" \
          SPIRA_AEON=1 \
          BEAD_ID="$BID" \
          BEADS_ACTOR="$ACTOR" \
          SPIRA_DB="$SPIRA_DB" \
          SPIRA_BD="$BD" \
          SPIRA_WORK="$REPO" \
          SPIRA_GH_API="$GH_API" \
          "$@" \
          bash "$GUARD" 2>&1); rc=$?
    printf '%s' "$out"
    return "$rc"
}

# ============================================================================
echo
echo "Setup: branch with workflow change"
# ============================================================================
branch_with_workflow

# ============================================================================
echo
echo "POSITIVE CONTROL — workflow changed, no URL → guard blocks"
# ============================================================================
rc=0; out="$(run_guard 2>&1 || true)"; run_guard >/dev/null 2>&1 || rc=$?
want   "guard blocks close without URL"            "BLOCKED by bd-close-workflow-run-guard" "$out"
want   "refusal mentions the law"                  "law-a-workflow-lands-on-its-own-run"    "$out"
want   "refusal gives the mail hint"               "mail.sh send operator"                  "$out"
wantrc "guard exits 2 to block the tool call"      2                                        "$rc"

# ============================================================================
echo
echo "Failing-open baseline — bd close without guard succeeds"
# ============================================================================
rc=0; BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" close "$BID" --reason "done" >/dev/null 2>&1 || rc=$?
wantrc "bd close without guard succeeds"           0                                        "$rc"
"$BD" -C "$SPIRA_DB" update "$BID" --status in_progress 2>/dev/null

# ============================================================================
echo
echo "URL with wrong head_branch → guard blocks"
# ============================================================================
stub_run testowner testrepo 111 \
    '{"head_branch":"some-other-branch","head_sha":"'"$HEAD_SHA"'","path":".github/workflows/acceptance.yml","status":"completed"}'

CLOSE_WRONG_BRANCH="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done https://github.com/testowner/testrepo/actions/runs/111
REASON"
rc=0
make_payload "$CLOSE_WRONG_BRANCH" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "wrong branch exits 2"                      2                                        "$rc"
out2="$(make_payload "$CLOSE_WRONG_BRANCH" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" 2>&1 || true)"
want "wrong branch message says WRONG_BRANCH"      "BLOCKED"                                "$out2"

# ============================================================================
echo
echo "URL with SHA not ancestor of HEAD → guard blocks"
# ============================================================================
stub_run testowner testrepo 222 \
    '{"head_branch":"test-wf-branch","head_sha":"0000000000000000000000000000000000000000","path":".github/workflows/acceptance.yml","status":"completed"}'

CLOSE_BAD_SHA="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done https://github.com/testowner/testrepo/actions/runs/222
REASON"
rc=0
make_payload "$CLOSE_BAD_SHA" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         SPIRA_BD_BRANCH="test-wf-branch" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "stale SHA exits 2"                         2                                        "$rc"

# ============================================================================
echo
echo "URL with wrong workflow file → guard blocks"
# ============================================================================
stub_run testowner testrepo 333 \
    '{"head_branch":"test-wf-branch","head_sha":"'"$HEAD_SHA"'","path":".github/workflows/gate.yml","status":"completed"}'

CLOSE_WRONG_WF="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done https://github.com/testowner/testrepo/actions/runs/333
REASON"
rc=0
make_payload "$CLOSE_WRONG_WF" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "wrong workflow exits 2"                    2                                        "$rc"

# ============================================================================
echo
echo "Run URL not found (404) → guard blocks"
# ============================================================================
# Run 999 has no stub, so stub server returns 404
CLOSE_404="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done https://github.com/testowner/testrepo/actions/runs/999
REASON"
rc=0
make_payload "$CLOSE_404" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "404 run exits 2"                           2                                        "$rc"

# ============================================================================
echo
echo "Valid URL (right branch, SHA is ancestor, right workflow) → guard allows"
# ============================================================================
# head_sha = BASE_SHA (ancestor of HEAD_SHA on test-wf-branch)
stub_run testowner testrepo 444 \
    '{"head_branch":"test-wf-branch","head_sha":"'"$BASE_SHA"'","path":".github/workflows/acceptance.yml","status":"completed"}'

CLOSE_VALID="bd -C $SPIRA_DB close $BID --reason-file - <<'REASON'
done https://github.com/testowner/testrepo/actions/runs/444
REASON"
rc=0
make_payload "$CLOSE_VALID" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "valid URL allows close (exits 0)"          0                                        "$rc"
out_valid="$(make_payload "$CLOSE_VALID" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" 2>&1 || true)"
nowant "valid URL not blocked"                     "BLOCKED"                                "$out_valid"

# ============================================================================
echo
echo "SPIRA_WORKFLOW_ONLY_PATHS script changed, no URL → guard blocks"
# ============================================================================
reset_to_base
branch_with_workflow_only
git -C "$REPO" checkout test-only-branch 2>/dev/null

BID2="$("$BD" -C "$SPIRA_DB" create --title "test-wf-only" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID2" ] || { printf 'SKIP could not create bead2\n'; exit 77; }
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID2" --claim 2>/dev/null

CLOSE2="bd -C $SPIRA_DB close $BID2 --reason-file - <<'REASON'
done no url
REASON"
rc=0
make_payload "$CLOSE2" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID2" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         SPIRA_WORKFLOW_ONLY_PATHS="spira/acceptance-ci.sh" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "workflow-only script triggers guard (exits 2)"  2                                  "$rc"

# ============================================================================
echo
echo "Only non-workflow file changed (spira/lib.sh) → guard allows"
# ============================================================================
reset_to_base
branch_with_lib
git -C "$REPO" checkout test-lib-branch 2>/dev/null

BID3="$("$BD" -C "$SPIRA_DB" create --title "test-lib-only" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID3" ] || { printf 'SKIP could not create bead3\n'; exit 77; }
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID3" --claim 2>/dev/null

CLOSE3="bd -C $SPIRA_DB close $BID3 --reason done-no-workflow-url"
rc=0
make_payload "$CLOSE3" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID3" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "lib-only change allows close (exits 0)"    0                                        "$rc"

# ============================================================================
echo
echo "SPIRA_WORKFLOW_RUN_CONSIDERED env override → guard allows"
# ============================================================================
git -C "$REPO" checkout test-wf-branch 2>/dev/null

BID4="$("$BD" -C "$SPIRA_DB" create --title "test-override" -l spira --type task \
         2>/dev/null | grep -oE 'sp-[a-z0-9]+' | head -1)"
[ -n "$BID4" ] || { printf 'SKIP could not create bead4\n'; exit 77; }
BEADS_ACTOR="$ACTOR" "$BD" -C "$SPIRA_DB" update "$BID4" --claim 2>/dev/null

CLOSE4="bd -C $SPIRA_DB close $BID4 --reason-file - <<'REASON'
done no url
REASON"
rc=0
make_payload "$CLOSE4" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID4" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         SPIRA_WORKFLOW_RUN_CONSIDERED="operator bypassed for test" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "env override allows close (exits 0)"       0                                        "$rc"

# ============================================================================
echo
echo "Inline SPIRA_WORKFLOW_RUN_CONSIDERED in command → guard allows"
# ============================================================================
CLOSE4_INLINE="SPIRA_WORKFLOW_RUN_CONSIDERED=deliberate bd -C $SPIRA_DB close $BID4 --reason done"
rc=0
make_payload "$CLOSE4_INLINE" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID4" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" SPIRA_GH_API="$GH_API" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "inline override allows close (exits 0)"    0                                        "$rc"

# ============================================================================
echo
echo "Non-aeon session → guard allows"
# ============================================================================
rc=0
make_payload "$CLOSE_CMD" | \
  env -i PATH="$PATH" HOME="$HOME" \
         BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" \
         SPIRA_WORK="$REPO" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "no SPIRA_AEON exits 0"                     0                                        "$rc"

# ============================================================================
echo
echo "Non-Bash tool call → guard allows"
# ============================================================================
rc=0
printf '{"tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}' | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "non-Bash tool exits 0"                     0                                        "$rc"

# ============================================================================
echo
echo "Non-close Bash command → guard allows"
# ============================================================================
rc=0
make_payload "bd -C $SPIRA_DB update $BID --status in_progress" | \
  env -i PATH="$PATH" HOME="$HOME" SPIRA_AEON=1 BEAD_ID="$BID" BEADS_ACTOR="$ACTOR" \
         SPIRA_DB="$SPIRA_DB" SPIRA_BD="$BD" SPIRA_WORK="$REPO" \
         bash "$GUARD" >/dev/null 2>&1 || rc=$?
wantrc "bd update not blocked exits 0"             0                                        "$rc"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
