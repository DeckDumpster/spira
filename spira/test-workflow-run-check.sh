#!/usr/bin/env bash
#
# test-workflow-run-check.sh — workflow-run-check.py (law-a-workflow-lands-on-its-own-run):
#   a branch touching .github/workflows/ or a workflow-only script must cite a dispatched
#   run URL, on that branch, whose head_sha is an ancestor of the tip and whose workflow
#   file matches what changed.
#
# THE STRUCTURE THIS REPLACES. A prior revision of this fence shipped as a second copy of
# this check wired into aeon_settings() as a PreToolUse hook (bd-close-workflow-run-guard.sh)
# — a hook that refuses a tool call, which is the exact shape retired everywhere else by
# sp-fjsxb per Ryan's 2026-09-23 "there are no guards" (brain CLAUDE.md: fix a re-violated
# rule structurally, not with a refusal). aeon.sh's own close-time teardown — which reopens a
# bead whose close does not meet this fence, the same shape as the cert-gate and
# close-reason-flags checks beside it — is the only enforcement now; this script is the one
# piece of logic both that teardown and this suite call, so they cannot disagree about which
# URL counts as evidence.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): prove the script prints
# NO_URL and exits 1 when a workflow file changed and no URL is in the close reason. If
# that does not fire, every "check allows" case below is vacuous.
#
# The API calls are intercepted via SPIRA_GH_API pointing at a stub HTTP server started by
# this suite (law-probe-a-fixture-not-production).
#
# tier: T2
# covers: spira/workflow-run-check.py aeon/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-workflow-run-check.sh"

CHECK="$HERE/workflow-run-check.py"
[ -f "$CHECK" ] || bail "workflow-run-check.py not found"

. "$HERE/testdb.sh"
testdb_require wf-run-check || exit 77
TMP="$(mktemp -d)"
trap 'testdb_drop 2>/dev/null; stop_stub; rm -rf "$TMP"' EXIT INT TERM
testdb_up wf-run-check || exit 1
BD="${SPIRA_BD:-bd}"

# ---- stub HTTP server, serves JSON fixtures at /repos/<owner>/<repo>/actions/runs/<id> ----
STUB_PID=""
stop_stub() { [ -n "$STUB_PID" ] && kill "$STUB_PID" 2>/dev/null || true; }
start_stub() {
    mkdir -p "$TMP/stub"
    python3 -c '
import http.server, os, sys

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
STUB_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')"
STUB_DIR="$TMP/stub"
STUB_DIR="$STUB_DIR" start_stub "$STUB_PORT"
GH_API="http://127.0.0.1:$STUB_PORT"
stub_run() { local key="${1}_${2}_actions_runs_${3}"; printf '%s' "$4" > "$STUB_DIR/repos_${key}.json"; }

# ---- git repo fixture, simulating a bead worktree -----------------------------------------
REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email "test@spira.local"
git -C "$REPO" config user.name "Test"
mkdir -p "$REPO/.github/workflows" "$REPO/spira"
printf 'name: Gate\n' > "$REPO/.github/workflows/gate.yml"
printf '# lib\n' > "$REPO/spira/lib.sh"
git -C "$REPO" add .
git -C "$REPO" commit -q -m "base"
BASE_SHA="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" branch -M main
git -C "$REPO" update-ref refs/remotes/origin/main "$BASE_SHA"

branch_with_workflow() {
    git -C "$REPO" checkout -q -b test-wf-branch
    printf 'name: Acceptance\n# changed\n' > "$REPO/.github/workflows/acceptance.yml"
    git -C "$REPO" add .github/workflows/acceptance.yml
    git -C "$REPO" commit -q -m "add acceptance workflow"
}
branch_with_workflow_only() {
    git -C "$REPO" checkout -q -b test-only-branch main
    printf '# acceptance-ci\n' > "$REPO/spira/acceptance-ci.sh"
    git -C "$REPO" add spira/acceptance-ci.sh
    git -C "$REPO" commit -q -m "update acceptance-ci"
}
branch_with_lib() {
    git -C "$REPO" checkout -q -b test-lib-branch main
    printf '# lib updated\n' > "$REPO/spira/lib.sh"
    git -C "$REPO" add spira/lib.sh
    git -C "$REPO" commit -q -m "update lib"
}
branch_with_workflow
HEAD_SHA="$(git -C "$REPO" rev-parse HEAD)"

# mkbead <title> -> a claimed bead id whose branch state is set to <branch>
mkbead() {
    local title="$1" branch="$2" bid
    # batch-job: fixture bd call against the suite's throwaway store
    bid="$("$BD" -C "$SPIRA_DB" create --title "$title" -l spira --type task 2>/dev/null \
            | grep -oE 'sp-[a-z0-9]+' | head -1)"
    [ -n "$bid" ] || bail "could not create test bead"
    "$BD" -C "$SPIRA_DB" update "$bid" --claim >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
    "$BD" -C "$SPIRA_DB" set-state "$bid" "branch=$branch" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
    printf '%s' "$bid"
}

# check <bid> <branch> [extra-env...] -> stdout, sets $rc
# NOTE (round 3 correction): workflow-run-check.py is a plain python script that reads
# SPIRA_BD/SPIRA_DB/SPIRA_GH_API/SPIRA_WORKFLOW_ONLY_PATHS via os.environ.get() directly —
# it was never ported to spira_config, so these are NOT read from $SPIRA_TOML despite being
# registered conf.d names elsewhere. tl_config (round 2's fix) was the wrong tool here: it
# only writes the override toml, which this consumer never looks at. Pass them as literal
# env vars, as this script has always expected.
CHECK_OUT=""
check() {
    local bid="$1" branch="$2"; shift 2
    CHECK_OUT="$(env -i PATH="$PATH" HOME="$HOME" \
        BEAD_ID="$bid" \
        SPIRA_REPO="$REPO" BRANCH="$branch" BASE="origin/main" \
        SPIRA_BD="$BD" SPIRA_DB="$SPIRA_DB" SPIRA_GH_API="$GH_API" \
        "$@" \
        python3 "$CHECK" 2>&1)"
}

# ============================================================================
echo
echo "POSITIVE CONTROL — workflow changed, no URL in close reason -> NO_URL"
# ============================================================================
BID1="$(mkbead wf-no-url test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID1" --reason "done without url" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID1" test-wf-branch
is "workflow changed, no URL prints NO_URL" "NO_URL" "$CHECK_OUT"
"$BD" -C "$SPIRA_DB" update "$BID1" --status in_progress >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store

# ============================================================================
echo
echo "URL with wrong head_branch"
# ============================================================================
stub_run testowner testrepo 111 \
    '{"head_branch":"some-other-branch","head_sha":"'"$HEAD_SHA"'","path":".github/workflows/acceptance.yml","status":"completed"}'
BID2="$(mkbead wf-wrong-branch test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID2" --reason "done https://github.com/testowner/testrepo/actions/runs/111" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID2" test-wf-branch
is "wrong branch prints WRONG_BRANCH" "WRONG_BRANCH:some-other-branch" "$CHECK_OUT"

# ============================================================================
echo
echo "URL whose head_sha is not an ancestor of the branch tip"
# ============================================================================
stub_run testowner testrepo 222 \
    '{"head_branch":"test-wf-branch","head_sha":"0000000000000000000000000000000000000000","path":".github/workflows/acceptance.yml","status":"completed"}'
BID3="$(mkbead wf-stale-sha test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID3" --reason "done https://github.com/testowner/testrepo/actions/runs/222" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID3" test-wf-branch
want "stale SHA prints SHA_NOT_ANCESTOR" "SHA_NOT_ANCESTOR:" "$CHECK_OUT"

# ============================================================================
echo
echo "URL for the wrong workflow file"
# ============================================================================
stub_run testowner testrepo 333 \
    '{"head_branch":"test-wf-branch","head_sha":"'"$HEAD_SHA"'","path":".github/workflows/gate.yml","status":"completed"}'
BID4="$(mkbead wf-wrong-file test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID4" --reason "done https://github.com/testowner/testrepo/actions/runs/333" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID4" test-wf-branch
is "wrong workflow prints WRONG_WORKFLOW" "WRONG_WORKFLOW:.github/workflows/gate.yml" "$CHECK_OUT"

# ============================================================================
echo
echo "Run URL not found (404)"
# ============================================================================
BID5="$(mkbead wf-404 test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID5" --reason "done https://github.com/testowner/testrepo/actions/runs/999" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID5" test-wf-branch
is "404 run prints RUN_NOT_FOUND:404" "RUN_NOT_FOUND:404" "$CHECK_OUT"

# ============================================================================
echo
echo "POSITIVE CONTROL, other side — valid URL prints OK"
# ============================================================================
stub_run testowner testrepo 444 \
    '{"head_branch":"test-wf-branch","head_sha":"'"$BASE_SHA"'","path":".github/workflows/acceptance.yml","status":"completed"}'
BID6="$(mkbead wf-valid test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID6" --reason "done https://github.com/testowner/testrepo/actions/runs/444" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID6" test-wf-branch
is "valid run (right branch, ancestor SHA, right file) prints OK" "OK" "$CHECK_OUT"

# ============================================================================
echo
echo "SPIRA_WORKFLOW_ONLY_PATHS script changed, no URL"
# ============================================================================
branch_with_workflow_only
BID7="$(mkbead wf-only-path test-only-branch)"
"$BD" -C "$SPIRA_DB" close "$BID7" --reason "done no url" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID7" test-only-branch SPIRA_WORKFLOW_ONLY_PATHS="spira/acceptance-ci.sh"
is "workflow-only script triggers NO_URL" "NO_URL" "$CHECK_OUT"

# ============================================================================
echo
echo "Only a non-workflow file changed (spira/lib.sh) -- fence does not fire"
# ============================================================================
branch_with_lib
BID8="$(mkbead wf-lib-only test-lib-branch)"
"$BD" -C "$SPIRA_DB" close "$BID8" --reason "done-no-workflow-url" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID8" test-lib-branch
is "lib-only change prints nothing (fence does not fire)" "" "$CHECK_OUT"

# ============================================================================
echo
echo "Agent-level override: bd note WORKFLOW_RUN_CONSIDERED: <reason>"
# ============================================================================
BID9="$(mkbead wf-override test-wf-branch)"
"$BD" -C "$SPIRA_DB" close "$BID9" --reason "done no url" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
"$BD" -C "$SPIRA_DB" note "$BID9" "WORKFLOW_RUN_CONSIDERED: operator bypassed for test" >/dev/null 2>&1 # batch-job: fixture bd call against the suite's throwaway store
check "$BID9" test-wf-branch
is "note override prints nothing (bypassed)" "" "$CHECK_OUT"

tl_summary
