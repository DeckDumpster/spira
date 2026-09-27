#!/usr/bin/env bash
# test-cargo-target-bins-prune.sh — cargo-target-bins-prune.sh: the sweep that bounds
# testenv-batch.sh's --with-bins cache (sp-uq6up). Nothing removed an entry there
# before, so one ~200MB directory accumulated per distinct tree ever tested — every
# certification, every round, every attribution run — until the disk filled twice in
# one day.
#
# WHAT THIS TESTS
#   A. a stale entry (older than the window) is removed, and reported.
#   B. a fresh entry (younger than the window) survives even though it is not the
#      tree about to be built.
#   C. the tree about to be built survives regardless of age.
#   D. an entry a concurrent run holds a shared flock on survives past the window —
#      the acceptance case from the bead — and is prunable once the lock releases.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   B and C each reuse the exact fixture shape that A/D prove gets removed, minus the
#   one property under test, so their survival proves that property protected the
#   entry rather than the sweep having simply found nothing to do.
#
# Pure filesystem + flock — no cargo, no podman, no network: the mechanism testenv-
# batch.sh's --with-bins block calls is exercised directly against a fixture cache
# dir, the same function, not a hand-written model of it.
#
# covers: spira/cargo-target-bins-prune.sh spira/testenv-batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

. "$HERE/testlib.sh"

command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

. "$HERE/cargo-target-bins-prune.sh"

echo "test-cargo-target-bins-prune.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
BASE="$TMP/cargo-target-bins"
mkdir -p "$BASE"

mk_entry() {  # mk_entry <tree> <age-seconds>
    local dir="$BASE/$1"
    mkdir -p "$dir"
    touch -d "$2 seconds ago" "$dir"
}

WINDOW=3600

# ---------------------------------------------------------------------------
echo
echo "A: a stale, unrelated, unlocked entry is removed"
# ---------------------------------------------------------------------------
mk_entry treeA $((WINDOW + 100))
out_a="$(_bins_prune_stale "$BASE" keeptree "$WINDOW")"
[ ! -d "$BASE/treeA" ] \
    && ok "A: stale entry is removed" \
    || bad "A: stale entry is removed" "directory still exists"
[[ "$out_a" == *"treeA"* ]] \
    && ok "A: removal is reported on stdout" \
    || bad "A: removal is reported on stdout" "got: $out_a"

# ---------------------------------------------------------------------------
echo
echo "B POSITIVE CONTROL: a fresh entry survives"
# ---------------------------------------------------------------------------
mk_entry treeB 10
_bins_prune_stale "$BASE" keeptree "$WINDOW" >/dev/null
[ -d "$BASE/treeB" ] \
    && ok "B: fresh entry survives — the age check is read, not skipped" \
    || bad "B: fresh entry survives" "directory was removed"

# ---------------------------------------------------------------------------
echo
echo "C POSITIVE CONTROL: the tree about to be built survives despite being stale"
# ---------------------------------------------------------------------------
mk_entry keeptree $((WINDOW + 100))
_bins_prune_stale "$BASE" keeptree "$WINDOW" >/dev/null
[ -d "$BASE/keeptree" ] \
    && ok "C: keep-tree survives regardless of age" \
    || bad "C: keep-tree survives" "directory was removed"

# ---------------------------------------------------------------------------
echo
echo "D ACCEPTANCE: an entry locked by a concurrent run survives past the window"
# ---------------------------------------------------------------------------
# A real --with-bins builder holds a SHARED flock on "<entry>.lock" for its whole
# build+copy; simulated here the way test-gate-sweep.sh simulates a held lock — open
# the fd, take the lock, keep it open across the call.
mk_entry treeD $((WINDOW + 100))
exec 8>"$BASE/treeD.lock"
flock -s 8
out_d="$(_bins_prune_stale "$BASE" keeptree "$WINDOW")"
[ -d "$BASE/treeD" ] \
    && ok "D: an entry locked by a concurrent run survives past the window" \
    || bad "D: locked entry survives" "directory was removed; sweep output: $out_d"
flock -u 8
exec 8>&-
_bins_prune_stale "$BASE" keeptree "$WINDOW" >/dev/null
[ ! -d "$BASE/treeD" ] \
    && ok "D+: once the lock releases, the next pass prunes it" \
    || bad "D+: lock release lets the entry be pruned" "directory still exists"

echo
tl_summary
