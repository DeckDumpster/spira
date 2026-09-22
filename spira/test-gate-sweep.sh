#!/usr/bin/env bash
#
# test-gate-sweep.sh — gate-sweep.sh removes stale .gate.<repo> worktrees and
# skips those whose lock is held.
#
#   ./test-gate-sweep.sh
#
# defect: sp-ic8n
# covers: spira/gate-sweep.sh spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"; SH="$TMP/spira"
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE/gate-sweep.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
printf 'base\n' > "$REPO/marker"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin

TREE="$RUN/worktree/.gate.$(basename "$REPO")"

echo "test-gate-sweep.sh — gate-sweep.sh removes stale gate worktrees"

run_sweep() {
    env -i HOME="$TMP/home" PATH="/usr/bin:/bin" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" \
        bash "$SH/gate-sweep.sh" "$REPO" "$@" 2>&1
}

# --------------------------------------------------------------------------------------
# CASE 1 — POSITIVE CONTROL. Create a stale /tmp worktree, then verify the sweep removes
# it. Without this, silence from the sweep is indistinguishable from it being pointed at
# the wrong repo (law-absence-needs-a-positive-control).
# --------------------------------------------------------------------------------------
# Simulate a /tmp-style orphan that an old gate left behind.
TMP_STALE="$TMP/tmp-style-gate/worktree/.gate.$(basename "$REPO")"
mkdir -p "$TMP_STALE"
git -C "$REPO" worktree add -q --detach "$TMP_STALE" HEAD
registered_before="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$TMP_STALE" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "orphan is registered before sweep" 1 "$registered_before"

# Redirect SPIRA_RUN so the sweep treats TMP_STALE as a non-$SPIRA_RUN path.
# The sweep removes worktrees named .gate.repo regardless of where they are; the /tmp
# check is a belt-and-suspenders path. We also set max-age=0 to catch it by age.
run_sweep 0 > /dev/null
registered_after="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$TMP_STALE" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "orphan is deregistered after sweep" 0 "$registered_after"
[ ! -d "$TMP_STALE" ] \
    && ok "orphan directory is removed after sweep" \
    || bad "orphan directory is removed after sweep" "directory still exists"

# --------------------------------------------------------------------------------------
# CASE 2 — SKIP LOCKED WORKTREES. A worktree whose lockfile is held must not be touched;
# a running gate holds that lock for the duration of its trial.
# --------------------------------------------------------------------------------------
git -C "$REPO" worktree add -q --detach "$TREE" HEAD
LOCKFILE="${TREE}.lock"
exec 8>"$LOCKFILE"
flock -x 8

out="$(exec 8>&-; run_sweep 0)"  # sweep with max-age=0; lock held by parent via fd 8
registered="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$TREE" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "locked worktree is not removed by sweep" 1 "$registered"
[[ "$out" == *"lock is held"* ]] \
    && ok "sweep reports that the lock is held" \
    || bad "sweep reports that the lock is held" "got: $out"

exec 8>&-
git -C "$REPO" worktree remove --force "$TREE" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# CASE 3 — YOUNG WORKTREES ARE KEPT. A gate worktree whose mtime is within the threshold
# and whose path is under SPIRA_RUN is not stale; the sweep must leave it alone.
# --------------------------------------------------------------------------------------
git -C "$REPO" worktree add -q --detach "$TREE" HEAD
# Default max-age is 3600s; newly created tree is seconds old.
run_sweep > /dev/null
registered="$(git -C "$REPO" worktree list --porcelain 2>/dev/null \
    | awk -v p="$TREE" '/^worktree /{if($2==p)c++} END{print c+0}')"
is "a young worktree is kept by sweep" 1 "$registered"
git -C "$REPO" worktree remove --force "$TREE" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# BATCH HOME SWEEP (sp-q7d72) — gate-sweep.sh removes /tmp/spira-batch-* homes that
# have no live container and are older than SPIRA_SUITE_TIMEOUT.
# --------------------------------------------------------------------------------------
echo
echo "Batch home sweep (sp-q7d72)"

PS_EMPTY="$TMP/ps-empty.txt"; touch "$PS_EMPTY"
PS_LIVE="$TMP/ps-live.txt"

run_sweep_batch() {
    env -i HOME="$TMP/home" PATH="/usr/bin:/bin" \
        SPIRA_CONF="$TMP/nonexistent.conf" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nonexistent-db" \
        SPIRA_SUITE_TIMEOUT="${1}" \
        SPIRA_PODMAN_PS_FILE="${2}" \
        bash "$SH/gate-sweep.sh" "$REPO" 2>&1
}

# CASE 4 — positive control: stale home, no live container → removed.
_BH_STALE="/tmp/spira-batch-sweeptest-stale-$$"
mkdir -p "$_BH_STALE"
touch -d "800 seconds ago" "$_BH_STALE"

_bsweep_out="$(run_sweep_batch 600 "$PS_EMPTY")"
[ ! -d "$_BH_STALE" ] \
    && ok "B1: stale batch home (dead container, old mtime) is removed" \
    || bad "B1: stale batch home (dead container, old mtime) is removed" "home still present; sweep output: $_bsweep_out"
rm -rf "$_BH_STALE" 2>/dev/null || true

[[ "$_bsweep_out" == *"spira-batch-sweeptest-stale-$$"* ]] \
    && ok "B1+: sweep logged the removal" \
    || bad "B1+: sweep logged the removal" "expected name in output; got: $_bsweep_out"

# CASE 5 — fresh home is not swept regardless of container state.
_BH_FRESH="/tmp/spira-batch-sweeptest-fresh-$$"
mkdir -p "$_BH_FRESH"
run_sweep_batch 600 "$PS_EMPTY" > /dev/null
[ -d "$_BH_FRESH" ] \
    && ok "B2: fresh batch home is not removed (age guard)" \
    || bad "B2: fresh batch home is not removed (age guard)" "home was removed"
rm -rf "$_BH_FRESH"

# CASE 6 — home with live container → not swept even when over threshold.
# Threshold is 0 so any non-zero age triggers the removal path; the container guard
# is what protects this one.  Short age (2s) keeps the fixture below the 600s
# threshold a concurrent gate uses, eliminating the race with test-gate-tree.sh.
_BH_LIVE_NAME="spira-batch-sweeptest-live-$$"
_BH_LIVE="/tmp/${_BH_LIVE_NAME}"
mkdir -p "$_BH_LIVE"
touch -d "2 seconds ago" "$_BH_LIVE"
printf '%s\n' "$_BH_LIVE_NAME" > "$PS_LIVE"
run_sweep_batch 0 "$PS_LIVE" > /dev/null
[ -d "$_BH_LIVE" ] \
    && ok "B3: home with live container is not removed" \
    || bad "B3: home with live container is not removed" "home was removed"
rm -rf "$_BH_LIVE"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
