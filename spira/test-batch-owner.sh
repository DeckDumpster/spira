#!/usr/bin/env bash
# test-batch-owner.sh — batch-owner.sh: the owner-file protocol that lets
# testenv-batch.sh's orphan sweep find a container after its owning process dies.
#
# WHAT THIS TESTS
#   A. _batch_owner_release: unlinks the owner file only once the container is
#      confirmed gone from podman — the fix for the leak path (sp-mxvdd), where a
#      failed teardown used to delete the owner file unconditionally.
#   B. _batch_sweep_dead_owners: owner file present, PID gone -> container reaped.
#   C. _batch_sweep_ownerless: a spira-batch-* container with NO owner file at all,
#      older than a caller-given bound -> reaped. This is the second leak route:
#      a container that lost its owner file (or never got one written before a
#      crash) was invisible to every sweep that only walks *.owner files.
#   D. INTEGRATION: the exact leak-then-sweep sequence from the bead — a "teardown"
#      that fails leaves the owner file and the container both in place, and the
#      next sweep pass reaps it.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   A0, B0, C3, C4: each sweep/release path is shown NOT to fire when its
#   condition is unmet, so a later "did not fire" result means the condition
#   really was unmet, not that the matcher is silently broken.
#
# Driven against real podman containers (docker.io/library/ubuntu:24.04, already
# used as the positive-control image in test-testenv.sh) rather than a hand-written
# model of container state — the discriminating fact here is what a real
# `podman container exists` / `podman ps` / `podman container inspect` says.
#
# host-reason: needs podman on PATH
# covers: spira/batch-owner.sh spira/testenv-batch.sh

set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-batch-owner.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-batch-owner.sh: podman not found on PATH\n' >&2
    exit 77
}

. "$HERE/batch-owner.sh"

IMG="docker.io/library/ubuntu:24.04"
TMP="$(mktemp -d)"
_ALL_CNAMES=""

_mk_container() {  # _mk_container <cname> — a real, long-lived, ownerless container
    podman run -d --name "$1" --rm "$IMG" sleep 300 >/dev/null 2>&1
    _ALL_CNAMES="$_ALL_CNAMES $1"
}

_cleanup() {
    for _c in $_ALL_CNAMES; do
        podman rm -f "$_c" >/dev/null 2>&1 || true
    done
    rm -f /tmp/spira-batch-own*-"$$".owner 2>/dev/null
    rm -rf "$TMP"
}
trap _cleanup EXIT INT TERM

# Positive control: prove podman itself can start this image before trusting any
# assertion below that depends on it (same control test-testenv.sh runs).
PC_NAME="batch-owner-pc-$$"
if podman run -d --name "$PC_NAME" --rm "$IMG" bash -c 'exit 0' >/dev/null 2>&1; then
    ok "positive control: podman can create a container from $IMG"
else
    bad "positive control: podman can create a container" "podman run failed on $IMG"
fi
podman rm -f "$PC_NAME" >/dev/null 2>&1 || true

# ===========================================================================
echo
echo "A: _batch_owner_release"
# ===========================================================================

CN_A="spira-batch-owna-$$"
_mk_container "$CN_A"
OF_A="$TMP/${CN_A}.owner"
printf '%s\n' 1 > "$OF_A"

# A0 POSITIVE CONTROL: the container still exists, so release must refuse and
# must leave the file in place — this is the exact bug: a container that
# survives teardown must not lose the one record that lets it be swept later.
if _batch_owner_release "$CN_A" "$OF_A"; then
    bad "A0: release refuses while the container still exists" "returned success"
else
    ok "A0: release refuses while the container still exists (rc=1)"
fi
[ -f "$OF_A" ] && ok "A0: owner file survives while the container still exists" \
                || bad "A0: owner file survives" "file was removed"

# A1: once the container is actually gone, release proceeds and removes the file.
podman rm -f "$CN_A" >/dev/null 2>&1
_ALL_CNAMES="${_ALL_CNAMES/ $CN_A/}"
if _batch_owner_release "$CN_A" "$OF_A"; then
    ok "A1: release proceeds once the container is confirmed gone"
else
    bad "A1: release proceeds once container is gone" "returned failure"
fi
[ ! -f "$OF_A" ] && ok "A1: owner file removed once the container is gone" \
                  || bad "A1: owner file removed" "file still present"

# ===========================================================================
echo
echo "B: _batch_sweep_dead_owners (owner file present, PID gone)"
# ===========================================================================

CN_B_LIVE="spira-batch-ownb-live-$$"
_mk_container "$CN_B_LIVE"
OF_B_LIVE="/tmp/${CN_B_LIVE}.owner"
printf '%s\n' "$$" > "$OF_B_LIVE"   # this test process is alive throughout

CN_B_DEAD="spira-batch-ownb-dead-$$"
_mk_container "$CN_B_DEAD"
OF_B_DEAD="/tmp/${CN_B_DEAD}.owner"
( exit 0 ) &
_dead_pid=$!
wait "$_dead_pid"
printf '%s\n' "$_dead_pid" > "$OF_B_DEAD"

_sweep_out_b="$(_batch_sweep_dead_owners)"

# B0 POSITIVE CONTROL: a live-PID owner file must NOT be swept.
if podman container exists "$CN_B_LIVE" 2>/dev/null; then
    ok "B0: positive-control: live-owner container is not swept"
else
    bad "B0: positive-control: live-owner container is not swept" "container was removed"
fi
[ -f "$OF_B_LIVE" ] && ok "B0: live-owner file survives" \
                     || bad "B0: live-owner file survives" "file was removed"
# Remove the container itself (not just the file) so it cannot be picked up,
# now ownerless, by the C tests below.
podman rm -f "$CN_B_LIVE" >/dev/null 2>&1 || true
rm -f "$OF_B_LIVE"
_ALL_CNAMES="${_ALL_CNAMES/ $CN_B_LIVE/}"

# B1: the dead-PID owner's container is reaped, and the sweep reports it.
if podman container exists "$CN_B_DEAD" 2>/dev/null; then
    bad "B1: dead-owner container is swept" "container still exists"
else
    ok "B1: dead-owner container is swept"
    _ALL_CNAMES="${_ALL_CNAMES/ $CN_B_DEAD/}"
fi
[ ! -f "$OF_B_DEAD" ] && ok "B1: dead-owner file is removed" \
                       || bad "B1: dead-owner file is removed" "file still present"
want "B1: sweep reports the container it removed" "$CN_B_DEAD" "$_sweep_out_b"

# ===========================================================================
echo
echo "C: _batch_sweep_ownerless (no owner file at all, age-bounded)"
# ===========================================================================

CN_C="spira-batch-ownc-$$"
_mk_container "$CN_C"
rm -f "/tmp/${CN_C}.owner"   # already absent, but explicit: this container is ownerless

# C3 POSITIVE CONTROL: a large min-age refuses to sweep a container that is
# in fact only seconds old — proves the age bound is read, not ignored.
_batch_sweep_ownerless 100000 >/dev/null
if podman container exists "$CN_C" 2>/dev/null; then
    ok "C3: positive-control: young ownerless container is not swept under a large min-age"
else
    bad "C3: positive-control: young container not swept under large min-age" \
        "container was removed — age bound is not being enforced"
fi

# CN_C4 has an owner file; it must be excluded from this arm even at min-age=0.
# Swept in the SAME pass as CN_C below, so the exclusion is proven while the
# sweep is actively reaping a sibling container, not merely when idle.
CN_C4="spira-batch-ownc4-$$"
_mk_container "$CN_C4"
printf '%s\n' "$$" > "/tmp/${CN_C4}.owner"

# C1 + C4: min-age=0 sweeps CN_C (ownerless) but must leave CN_C4 (owner file
# present) alone in the same pass.
_sweep_out_c="$(_batch_sweep_ownerless 0)"

if podman container exists "$CN_C" 2>/dev/null; then
    bad "C1: ownerless container is swept at min-age=0" "container still exists"
else
    ok "C1: ownerless container is swept at min-age=0"
    _ALL_CNAMES="${_ALL_CNAMES/ $CN_C/}"
fi
want "C1: sweep reports the container it removed" "$CN_C" "$_sweep_out_c"

# C4 POSITIVE CONTROL: an owner file present excludes the container from this
# arm even at min-age=0 — arm 2 must defer entirely to arm 1 when a file exists.
if podman container exists "$CN_C4" 2>/dev/null; then
    ok "C4: positive-control: owner-file-present container excluded from ownerless arm"
else
    bad "C4: positive-control: owner-file-present container excluded" \
        "container was removed — arm 2 did not defer to the owner file"
fi
rm -f "/tmp/${CN_C4}.owner"
podman rm -f "$CN_C4" >/dev/null 2>&1 || true
_ALL_CNAMES="${_ALL_CNAMES/ $CN_C4/}"

# ===========================================================================
echo
echo "D: integration — the leak path from the bead"
# ===========================================================================
# 1. A container is up. 2. Teardown ('testenv down') fails — simulated with a
#    bare failing command, since the decision under test is made by
#    _batch_owner_release from podman state, not from down's own exit code.
# 3. The owner file must survive, and the container must still be there.
# 4. The next sweep pass (the one testenv-batch.sh runs before every new batch)
#    reaps it.

CN_D="spira-batch-ownd-$$"
_mk_container "$CN_D"
OF_D="/tmp/${CN_D}.owner"
( exit 0 ) &
_d_dead_pid=$!
wait "$_d_dead_pid"
printf '%s\n' "$_d_dead_pid" > "$OF_D"

# No real "testenv down" call is made: the release decision is made from podman
# state alone (see batch-owner.sh), so a failed teardown is exercised simply by
# never having stopped or removed the container before calling release.
_batch_owner_release "$CN_D" "$OF_D" \
    && bad "D1: release does not proceed while the container survived teardown" "returned success" \
    || ok "D1: release refuses while the container survived teardown"

[ -f "$OF_D" ] && ok "D2: owner file survives the failed teardown" \
                || bad "D2: owner file survives the failed teardown" "file was removed"
podman container exists "$CN_D" 2>/dev/null \
    && ok "D3: container survives the failed teardown" \
    || bad "D3: container survives the failed teardown" "container is gone"

_sweep_out_d="$(_batch_sweep_dead_owners)"
podman container exists "$CN_D" 2>/dev/null \
    && bad "D4: the next sweep reaps the surviving container" "container still exists" \
    || { ok "D4: the next sweep reaps the surviving container"; _ALL_CNAMES="${_ALL_CNAMES/ $CN_D/}"; }
[ ! -f "$OF_D" ] && ok "D5: the sweep removes the owner file too" \
                  || bad "D5: the sweep removes the owner file" "file still present"
want "D6: sweep reports the reaped container" "$CN_D" "$_sweep_out_d"

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
