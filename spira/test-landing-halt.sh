#!/usr/bin/env bash
# test-landing-halt.sh — landing.sh halt subcommand: clean pass termination
#
# Acceptance criteria (sp-wbany):
#   • --dry-run on an idle system reports "no pass running" and exits non-zero.
#   • halt on an idle system exits non-zero.
#   • halt signals the running pass and writes an interrupt record before returning.
#   • halt cleans up containers it recorded in SPIRA_LANDING_CONTAINERS.
#   • halt removes unpushed batch branches (spira/queue/*) with no remote tracking ref.
#
# POSITIVE CONTROL: each structural check is first proved against an offender before
# trusting the passing case (law-a-regression-test-must-be-seen-to-fail).
#
# covers: spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { printf '%s' "$3" | grep -qF "$2" && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { ! printf '%s' "$3" | grep -qF "$2" && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-landing-halt.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

SPIRA_RUN="$TMP/run"
mkdir -p "$SPIRA_RUN"

LAND_RUN="$SPIRA_RUN/landing.run"
LAND_CONTAINERS="$SPIRA_RUN/landing.containers"
SPIRA_REPO_MAP="$TMP/repo-map"

# Minimal conf seam: SPIRA_CONF points nowhere so lib.sh uses defaults.
# SPIRA_REPO_MAP is passed explicitly; before the file exists repo_names() returns nothing.
run_halt() {
    env -i PATH="$PATH" HOME="$HOME" \
        SPIRA_RUN="$SPIRA_RUN" \
        SPIRA_HOME="$HERE" \
        SPIRA_PROD="$HERE" \
        SPIRA_CONF=/nonexistent \
        SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
        bash "$HERE/landing.sh" halt "$@" 2>&1
}

# ===========================================================================
echo
echo "dry-run on idle system"
# ===========================================================================

out="$(run_halt --dry-run 2>&1)"; rc=$?
is   "dry-run idle: exits 1"                   "1" "$rc"
want "dry-run idle: says no pass running"       "no pass running" "$out"

out="$(run_halt --dry-run 2>&1)"; rc=$?
is   "dry-run idle: non-zero on second call"    "1" "$rc"

# ===========================================================================
echo
echo "halt on idle system"
# ===========================================================================

out="$(run_halt 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "halt idle: exits non-zero" \
    || bad "halt idle: exits non-zero" "exit $rc"
want "halt idle: says nothing to halt" "nothing to halt" "$out"

# ===========================================================================
echo
echo "dry-run with a running process"
# ===========================================================================

# Stand in for a landing pass with a long-lived process.
sleep 300 &
FAKE_PID=$!
trap 'kill "$FAKE_PID" 2>/dev/null; rm -rf "$TMP"' EXIT

printf 'pid=%s\nstarted=%s\nrepo=testrepo\nbranch=spira/sp-test\nphase=gate\n' \
    "$FAKE_PID" "$(( $(date +%s) - 60 ))" > "$LAND_RUN"

out="$(run_halt --dry-run 2>&1)"; rc=$?
is   "dry-run running: exits 0"         "0" "$rc"
want "dry-run running: shows pid"       "pid=$FAKE_PID" "$out"
want "dry-run running: shows repo"      "repo=testrepo" "$out"
want "dry-run running: shows phase"     "phase=gate" "$out"
want "dry-run running: shows elapsed"   "elapsed=" "$out"

# Container in registry shows up in dry-run.
printf 'spira-batch-testcont1\n' > "$LAND_CONTAINERS"
out="$(run_halt --dry-run 2>&1)"; rc=$?
want "dry-run: lists container to tear down" "spira-batch-testcont1" "$out"

# ===========================================================================
echo
echo "halt signals the process and writes interrupt record"
# ===========================================================================

sleep 300 &
HALT_PID=$!

printf 'pid=%s\nstarted=%s\nrepo=spira\nbranch=spira/sp-abc\nphase=gate\n' \
    "$HALT_PID" "$(( $(date +%s) - 120 ))" > "$LAND_RUN"
: > "$LAND_CONTAINERS"

out="$(run_halt --reason "test halt reason" 2>&1)"; rc=$?
is "halt exits 0" "0" "$rc"
want "halt: output names repo"          "repo=spira"    "$out"
want "halt: output names phase"         "phase=gate"    "$out"
want "halt: output records reason"      "test halt reason" "$out"

# Process must be gone after halt.
if [ -d "/proc/$HALT_PID" ]; then
    bad "halt: process still running after halt" "pid $HALT_PID still in /proc"
    kill "$HALT_PID" 2>/dev/null || true
else
    ok "halt: process was killed"
fi

# Interrupt record must exist with the right fields.
int_file="$SPIRA_RUN/landing.interrupted"
[ -f "$int_file" ] && ok "halt: interrupt record written" \
    || bad "halt: interrupt record written" "file $int_file not found"
int_out="$(cat "$int_file" 2>/dev/null)"
want "halt: interrupt has reason"   "test halt reason" "$int_out"
want "halt: interrupt has repo"     "repo=spira"       "$int_out"
want "halt: interrupt has branch"   "spira/sp-abc"     "$int_out"
want "halt: interrupt has halted="  "halted="          "$int_out"

# State file must be gone after halt.
[ ! -f "$LAND_RUN" ] && ok "halt: state file removed" \
    || bad "halt: state file removed" "LAND_RUN still exists"

# ===========================================================================
echo
echo "orphaned batch branch cleanup"
# ===========================================================================

# Create a minimal git repo simulating a queue-mode repository.
BATCHREPO="$TMP/batchrepo"
git init -q "$BATCHREPO"
git -C "$BATCHREPO" commit --allow-empty -q -m "base"

git -C "$BATCHREPO" branch "spira/queue/20260917T215100Z"
git -C "$BATCHREPO" branch "spira/queue/20260917T220000Z"

# Simulate spira_repos() and repo_land() returning this repo in queue mode.
# Override via env vars that lib.sh/conf.sh expose.
printf 'spira-test | %s | queue | refs/remotes/origin/main | | \n' "$BATCHREPO" \
    > "$SPIRA_REPO_MAP"

# Write a fake open batch record for one of the branches (simulating it was pushed).
QUEUE_DIR="$SPIRA_RUN/queue"
mkdir -p "$QUEUE_DIR/spira-test"
printf 'branch=spira/queue/20260917T220000Z\npr=42\n' \
    > "$QUEUE_DIR/spira-test/open"

sleep 300 &
BATCH_PID=$!

printf 'pid=%s\nstarted=%s\nrepo=spira-test\nbranch=spira/queue/20260917T215100Z\nphase=queue\n' \
    "$BATCH_PID" "$(date +%s)" > "$LAND_RUN"
: > "$LAND_CONTAINERS"

out="$(run_halt 2>&1)"; rc=$?
kill "$BATCH_PID" 2>/dev/null || true

# The orphaned branch (no open record, no remote ref) should be removed.
if ! git -C "$BATCHREPO" show-ref --verify -q "refs/heads/spira/queue/20260917T215100Z" 2>/dev/null; then
    ok "halt: orphaned batch branch removed"
else
    bad "halt: orphaned batch branch removed" "branch still exists"
fi

# The open-record branch should NOT be touched (it may be pushed).
if git -C "$BATCHREPO" show-ref --verify -q "refs/heads/spira/queue/20260917T220000Z" 2>/dev/null; then
    ok "halt: open batch branch preserved"
else
    bad "halt: open batch branch preserved" "branch was deleted"
fi

# ===========================================================================
echo
echo "halt tears down a real container recorded in the registry (gap G9)"
# ===========================================================================
# Every case above empties LAND_CONTAINERS before the real (non-dry-run) halt
# runs, so only the dry-run "would tear down" listing (above) ever reads a
# populated registry — the real teardown call (podman ps + testenv.sh down)
# has never actually run. Stub both so this exercises the real code path
# rather than re-testing the listing.
CONT_NAME="spira-batch-realcont1"
BIN_DIR="$TMP/bin"; mkdir -p "$BIN_DIR"
cat > "$BIN_DIR/podman" <<PODEOF
#!/usr/bin/env bash
[ "\$1" = "ps" ] && printf '%s\n' "$CONT_NAME"
exit 0
PODEOF
chmod +x "$BIN_DIR/podman"

PROD_DIR="$TMP/prod"; mkdir -p "$PROD_DIR"
TEARDOWN_LOG="$TMP/teardown.log"; rm -f "$TEARDOWN_LOG"
cat > "$PROD_DIR/testenv.sh" <<TDEOF
#!/usr/bin/env bash
if [ "\$1" = "down" ]; then
    printf '%s\n' "\$3" >> "$TEARDOWN_LOG"
fi
exit 0
TDEOF
chmod +x "$PROD_DIR/testenv.sh"

sleep 300 &
CONT_PID=$!
printf 'pid=%s\nstarted=%s\nrepo=spira\nbranch=spira/sp-cont\nphase=gate\n' \
    "$CONT_PID" "$(date +%s)" > "$LAND_RUN"
printf '%s\n' "$CONT_NAME" > "$LAND_CONTAINERS"

out="$(env -i PATH="$BIN_DIR:$PATH" HOME="$HOME" \
    SPIRA_RUN="$SPIRA_RUN" \
    SPIRA_HOME="$HERE" \
    SPIRA_PROD="$PROD_DIR" \
    SPIRA_CONF=/nonexistent \
    SPIRA_REPO_MAP="$SPIRA_REPO_MAP" \
    bash "$HERE/landing.sh" halt --reason "container teardown test" 2>&1)"; rc=$?
kill "$CONT_PID" 2>/dev/null || true

is   "container-halt: exits 0"                            "0" "$rc"
want "container-halt: reports tearing down the container" "tearing down container $CONT_NAME" "$out"
if [ -f "$TEARDOWN_LOG" ] && grep -qxF "$CONT_NAME" "$TEARDOWN_LOG"; then
    ok "container-halt: testenv.sh down --name was actually invoked"
else
    bad "container-halt: testenv.sh down --name was actually invoked" "no matching line in $TEARDOWN_LOG"
fi

# ===========================================================================
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
