#!/usr/bin/env bash
#
# test-parallel-isolation.sh — per-suite HOME isolation in testenv-batch.sh.
#
# WHAT THIS PROVES
#   testenv-batch.sh gives each parallel suite its own HOME so that unit fixture
#   files written by one suite to $HOME/.config/systemd/user are not visible to
#   neighbouring suites. Before this fix, all parallel suites shared the container
#   user's real home and a fixture written by suite A appeared in suite B's DEST scan.
#
# POSITIVE CONTROL (law-a-regression-test-must-be-seen-to-fail)
#   Part A directly injects two suites into one container with SHARED HOME —
#   the checker suite must see the planter's canary and exit 1. Only then does
#   Part B's per-suite HOME result carry weight.
#
# POSITIVE CONTROL SEEN RED (before this fix):
#   Without per-suite HOME, running the same fixture suites via testenv-batch.sh
#   in parallel produced:
#     test-fx-checker.sh: FAIL  no canary expected: found canary-planted.service
#   (rc=1; the planter ran concurrently and left its file in the shared home)
#
# host-reason: Part A plants and reads inside a live container; Part B drives
#              testenv-batch.sh, which starts its own container.
#
# covers: spira/testenv-batch.sh spira/testenv.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "expected 0, got $2"; }
isexit1() { [ "$2" = 1 ] && ok "$1" || bad "$1" "expected 1, got $2"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
notwant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
isfile()  { [ -f "$2" ] && ok "$1" || bad "$1" "file not found: $2"; }

BATCH="$HERE/testenv-batch.sh"
TESTENV="$HERE/testenv.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "test-parallel-isolation.sh"

# find_results_dir: locate the batch.meta file under a results root.
find_results_dir() {
    find "$1" -maxdepth 2 -name batch.meta 2>/dev/null | head -1 | xargs dirname 2>/dev/null || true
}

# ===========================================================================
# PART A: POSITIVE CONTROL — shared HOME causes the checker to see the
# planter's canary. This establishes that the isolation is needed.
# ===========================================================================
echo
echo "Part A: positive control — shared HOME (no isolation)"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-parallel-isolation.sh: podman not on PATH\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

CTRL_CNAME="spira-par-ctrl-$$"
_ctrl_cleanup() { podman stop "$CTRL_CNAME" >/dev/null 2>&1 || true; podman rm "$CTRL_CNAME" >/dev/null 2>&1 || true; }
trap '_ctrl_cleanup; rm -rf "$TMP"' EXIT

printf 'par-isolation: starting control container...\n' >&2
bash "$TESTENV" up --name "$CTRL_CNAME" >&2 || {
    printf 'SKIP test-parallel-isolation.sh Part A: control container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}

_SPIRA_USER=spirauser
_SPIRA_UID=1001
_USER_RUNTIME="/run/user/${_SPIRA_UID}"
# The container user's real home directory.
_REAL_HOME="/home/${_SPIRA_USER}"

# SHARED_HOME: both suites use the real home, simulating the pre-fix state.
SHARED_HOME="${_REAL_HOME}"

# Suite: planter — writes a canary file into the shared HOME's systemd unit dir.
PLANTER_SCRIPT='#!/usr/bin/env bash
mkdir -p "$HOME/.config/systemd/user"
printf "[Unit]\nDescription=canary\n" > "$HOME/.config/systemd/user/canary-planted.service"
printf "  ok    planter: canary written to %s\n" "$HOME/.config/systemd/user/canary-planted.service"
exit 0'

# Suite: checker — asserts NO canary-*.service exists in HOME's systemd unit dir.
CHECKER_SCRIPT='#!/usr/bin/env bash
found="$(ls "$HOME/.config/systemd/user/canary-"*.service 2>/dev/null || true)"
if [ -n "$found" ]; then
    printf "  FAIL  checker: unexpected canary found: %s\n" "$found"; exit 1
fi
printf "  ok    checker: no canary in %s\n" "$HOME/.config/systemd/user"
exit 0'

# Run the planter first with SHARED_HOME.
podman exec --user "$_SPIRA_USER" \
    -e "HOME=${SHARED_HOME}" \
    -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
    "$CTRL_CNAME" bash -c "$PLANTER_SCRIPT" >/dev/null 2>&1 || true

# Run the checker with the SAME SHARED_HOME — must find the canary (positive control).
_ctrl_rc=0
podman exec --user "$_SPIRA_USER" \
    -e "HOME=${SHARED_HOME}" \
    -e "XDG_RUNTIME_DIR=${_USER_RUNTIME}" \
    "$CTRL_CNAME" bash -c "$CHECKER_SCRIPT" >/dev/null 2>&1 || _ctrl_rc=$?

isexit1 "A1: checker exits 1 when sharing HOME with planter (canary visible)" "$_ctrl_rc"

# Clean up the control container before Part B starts its own.
_ctrl_cleanup

# ===========================================================================
# PART B: PER-SUITE HOME — testenv-batch.sh gives each parallel suite its own
# HOME; the checker must not see the planter's canary.
# ===========================================================================
echo
echo "Part B: per-suite HOME isolation via testenv-batch.sh"

# Pre-flight: confirm the image and user systemd are usable.
PRE_CNAME="spira-par-pre-$$"
printf 'par-isolation: pre-flight container check...\n' >&2
bash "$TESTENV" up --name "$PRE_CNAME" >&2 || {
    printf 'SKIP test-parallel-isolation.sh Part B: container did not start\n' >&2
    [ "$fail" -gt 0 ] && exit 1; exit 77
}
bash "$TESTENV" down --name "$PRE_CNAME" >/dev/null 2>&1 || true
ok "B0: pre-flight: container available"

# Fixture repo: minimal git repo so testenv-batch.sh can resolve a base ref.
REMOTE="$TMP/remote"
FIXTURE="$TMP/fixture"
git init -q --initial-branch=master "$REMOTE"
git -C "$REMOTE" config user.email "test@spira.local"
git -C "$REMOTE" config user.name "Spira Test"
touch "$REMOTE/placeholder"
git -C "$REMOTE" add placeholder
git -C "$REMOTE" commit -q -m "initial"
git clone -q --local "$REMOTE" "$FIXTURE"
git -C "$FIXTURE" config user.email "test@spira.local"
git -C "$FIXTURE" config user.name "Spira Test"

# Topic branch: a trivial change so the batch sees changed files.
git -C "$FIXTURE" checkout -q -b topic
printf 'changed\n' > "$FIXTURE/changed.txt"
git -C "$FIXTURE" add changed.txt
git -C "$FIXTURE" commit -q -m "add changed.txt"

mkdir -p "$FIXTURE/spira"
SUITE_B="$TMP/suites-B"
mkdir -p "$SUITE_B"

# Suite: planter — writes a canary file to $HOME/.config/systemd/user and exits 0.
cat > "$SUITE_B/test-fx-planter.sh" << 'PLANTER'
#!/usr/bin/env bash
# covers: changed.txt
mkdir -p "$HOME/.config/systemd/user"
printf "[Unit]\nDescription=canary\n" \
    > "$HOME/.config/systemd/user/canary-planted.service"
printf "  ok    planter: canary written to %s\n" "$HOME/.config/systemd/user/canary-planted.service"
exit 0
PLANTER
chmod +x "$SUITE_B/test-fx-planter.sh"
cp "$SUITE_B/test-fx-planter.sh" "$FIXTURE/spira/test-fx-planter.sh"

# Suite: checker — asserts NO canary-*.service exists in $HOME/.config/systemd/user.
# With per-suite HOME, HOME is a clean tmpfs; no canary is present.
cat > "$SUITE_B/test-fx-checker.sh" << 'CHECKER'
#!/usr/bin/env bash
# covers: changed.txt
mkdir -p "$HOME/.config/systemd/user"
found="$(ls "$HOME/.config/systemd/user/canary-"*.service 2>/dev/null || true)"
if [ -n "$found" ]; then
    printf "  FAIL  checker: unexpected canary found: %s\n" "$found"
    exit 1
fi
printf "  ok    checker: HOME is isolated (%s)\n" "$HOME"
exit 0
CHECKER
chmod +x "$SUITE_B/test-fx-checker.sh"
cp "$SUITE_B/test-fx-checker.sh" "$FIXTURE/spira/test-fx-checker.sh"

# ---------------------------------------------------------------------------
# B1: ISOLATION — run planter and checker in parallel; checker must pass.
# SPIRA_BATCH_MAXPAR=0 to confirm isolation works regardless of throttle.
# ---------------------------------------------------------------------------
echo
echo "B1: parallel isolation (both suites run concurrently)"

RESULTS_ROOT_B1="$TMP/results-B1"
rc_b1=0
SPIRA_BATCH_SUITE_DIR="$SUITE_B" \
SPIRA_BATCH_RESULTS="$RESULTS_ROOT_B1" \
SPIRA_BATCH_SKIP_INSTALL=1 \
SPIRA_BATCH_INSTANCE="b1-$$" \
SPIRA_BATCH_MAXPAR=0 \
    bash "$BATCH" --mode parallel topic "$FIXTURE" || rc_b1=$?

iszero "B1: batch exits 0 (both suites pass)" "$rc_b1"

RD_B1="$(find_results_dir "$RESULTS_ROOT_B1")"
if [ -n "$RD_B1" ]; then
    if [ -f "$RD_B1/test-fx-checker.sh.result" ]; then
        _st="$(awk '{print $1}' "$RD_B1/test-fx-checker.sh.result")"
        [ "$_st" = ok ] \
            && ok "B1: checker suite passed (HOME is isolated — no cross-contamination)" \
            || bad "B1: checker suite passed" "got status=$_st (isolation not working)"
    else
        bad "B1: checker result file present" "missing"
    fi
    if [ -f "$RD_B1/test-fx-planter.sh.result" ]; then
        _st="$(awk '{print $1}' "$RD_B1/test-fx-planter.sh.result")"
        [ "$_st" = ok ] \
            && ok "B1: planter suite passed" \
            || bad "B1: planter suite passed" "got status=$_st"
    fi
    # Confirm both ran in parallel mode, not serial.
    if [ -f "$RD_B1/test-fx-checker.sh.result" ]; then
        _mode="$(awk '{print $5}' "$RD_B1/test-fx-checker.sh.result")"
        [ "$_mode" = parallel ] \
            && ok "B1: checker ran in parallel mode" \
            || bad "B1: checker ran in parallel mode" "mode field: $_mode"
    fi
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
