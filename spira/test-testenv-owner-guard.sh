#!/usr/bin/env bash
# test-testenv-owner-guard.sh — testenv.sh down refuses to tear down a container owned by
# a live process that is not the caller or one of its ancestors (law-guard-binds-the-caller).
#
# WHAT THIS PROVES (sp-scgr2)
#   A. POSITIVE CONTROL: podman can create the containers this suite depends on.
#   B. A live foreign owner blocks `down`: the container survives, the call exits non-zero.
#   C. The owner's own teardown (owner pid is caller-or-ancestor) succeeds.
#   D. A dead owner pid does not block `down` — a stale owner file is not a lock forever.
#   E. --force-foreign overrides a live foreign owner.
#   F. No owner file at all — the pre-existing, unowned-container case — is unaffected.
#
# Driven against real podman containers (docker.io/library/ubuntu:24.04, the same image
# test-testenv.sh and test-batch-owner.sh already use for positive controls) rather than a
# hand-written model of "container exists" — the discriminating fact is what a real `podman
# container exists` says, and the container this suite must NOT be allowed to remove.
#
# SEEN TO FAIL AGAINST UNFIXED TREE: before this bead, `testenv.sh down --name X` stopped
# and removed X unconditionally, from any caller — B and E below both passed as a no-op
# success against the prior code, which is exactly how one aeon's `for n in $(podman ps -a
# ...); do testenv.sh down --name "$n"; done` stopped every other aeon's and the Concierge's
# containers on the host.
#
# host-reason: needs podman on PATH
# covers: spira/testenv.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-testenv-owner-guard.sh"

command -v podman >/dev/null 2>&1 || {
    printf 'SKIP test-testenv-owner-guard.sh: podman not found on PATH\n' >&2
    exit 77
}

TESTENV="$HERE/testenv.sh"
IMG="docker.io/library/ubuntu:24.04"
_ALL_CNAMES=""

_mk_container() {  # _mk_container <cname> — a real, long-lived container
    podman run -d --name "$1" --rm "$IMG" sleep 300 >/dev/null 2>&1
    _ALL_CNAMES="$_ALL_CNAMES $1"
}

_cleanup() {
    for _c in $_ALL_CNAMES; do
        podman rm -f "$_c" >/dev/null 2>&1 || true
    done
    rm -f /tmp/spira-teg-*-$$.owner 2>/dev/null
}
trap _cleanup EXIT INT TERM

# ===========================================================================
echo
echo "A: positive control"
# ===========================================================================
PC_NAME="spira-teg-pc-$$"
if podman run -d --name "$PC_NAME" --rm "$IMG" bash -c 'exit 0' >/dev/null 2>&1; then
    ok "positive control: podman can create a container from $IMG"
else
    bad "positive control: podman can create a container" "podman run failed on $IMG"
fi
podman rm -f "$PC_NAME" >/dev/null 2>&1 || true

# ===========================================================================
echo
echo "B: a live foreign owner blocks down"
# ===========================================================================
CN_B="spira-teg-b-$$"
_mk_container "$CN_B"
OF_B="/tmp/${CN_B}.owner"

# A sibling process this test spawned, alive throughout — not this process,
# and not an ancestor of it. Exactly the "unrelated shell" from the bead.
sleep 300 &
FOREIGN_PID=$!
printf '%s\n' "$FOREIGN_PID" > "$OF_B"

if bash "$TESTENV" down --name "$CN_B" >/dev/null 2>&1; then
    bad "B1: down refuses a live foreign owner" "down exited 0"
else
    ok "B1: down refuses a live foreign owner (non-zero exit)"
fi
if podman container exists "$CN_B" 2>/dev/null; then
    ok "B2: the container survives"
else
    bad "B2: the container survives" "container was removed despite the refusal"
fi
[ -f "$OF_B" ] && ok "B3: the owner file survives" \
                || bad "B3: the owner file survives" "file was removed"

kill "$FOREIGN_PID" 2>/dev/null || true
wait "$FOREIGN_PID" 2>/dev/null || true

# ===========================================================================
echo
echo "C: the owner's own teardown succeeds"
# ===========================================================================
CN_C="spira-teg-c-$$"
_mk_container "$CN_C"
OF_C="/tmp/${CN_C}.owner"

# This test script's own pid is an ancestor of the `down` subprocess it is
# about to spawn (bash "$TESTENV" down ... runs as its child) — the same
# relationship testenv-batch.sh's EXIT trap has to the container it started.
printf '%s\n' "$$" > "$OF_C"

if bash "$TESTENV" down --name "$CN_C" >/dev/null 2>&1; then
    ok "C1: down succeeds for the owner (or a descendant of it)"
else
    bad "C1: down succeeds for the owner" "down exited non-zero"
fi
if podman container exists "$CN_C" 2>/dev/null; then
    bad "C2: the container is gone" "container still exists"
else
    ok "C2: the container is gone"
    _ALL_CNAMES="${_ALL_CNAMES/ $CN_C/}"
fi
[ ! -f "$OF_C" ] && ok "C3: the owner file is released" \
                  || bad "C3: the owner file is released" "file still present"

# ===========================================================================
echo
echo "D: a dead owner pid does not block down"
# ===========================================================================
CN_D="spira-teg-d-$$"
_mk_container "$CN_D"
OF_D="/tmp/${CN_D}.owner"

( exit 0 ) &
DEAD_PID=$!
wait "$DEAD_PID"
printf '%s\n' "$DEAD_PID" > "$OF_D"

if bash "$TESTENV" down --name "$CN_D" >/dev/null 2>&1; then
    ok "D1: down proceeds when the owner pid is dead"
else
    bad "D1: down proceeds when the owner pid is dead" "down exited non-zero"
fi
if podman container exists "$CN_D" 2>/dev/null; then
    bad "D2: the container is gone" "container still exists"
else
    ok "D2: the container is gone"
    _ALL_CNAMES="${_ALL_CNAMES/ $CN_D/}"
fi

# ===========================================================================
echo
echo "E: --force-foreign overrides a live foreign owner"
# ===========================================================================
CN_E="spira-teg-e-$$"
_mk_container "$CN_E"
OF_E="/tmp/${CN_E}.owner"

sleep 300 &
FOREIGN_PID_E=$!
printf '%s\n' "$FOREIGN_PID_E" > "$OF_E"

if bash "$TESTENV" down --name "$CN_E" --force-foreign >/dev/null 2>&1; then
    ok "E1: --force-foreign proceeds despite a live foreign owner"
else
    bad "E1: --force-foreign proceeds despite a live foreign owner" "down exited non-zero"
fi
if podman container exists "$CN_E" 2>/dev/null; then
    bad "E2: the container is gone" "container still exists"
else
    ok "E2: the container is gone"
    _ALL_CNAMES="${_ALL_CNAMES/ $CN_E/}"
fi

kill "$FOREIGN_PID_E" 2>/dev/null || true
wait "$FOREIGN_PID_E" 2>/dev/null || true

# ===========================================================================
echo
echo "F: no owner file at all — unaffected"
# ===========================================================================
CN_F="spira-teg-f-$$"
_mk_container "$CN_F"
rm -f "/tmp/${CN_F}.owner"   # already absent, but explicit

if bash "$TESTENV" down --name "$CN_F" >/dev/null 2>&1; then
    ok "F1: down proceeds with no owner file"
else
    bad "F1: down proceeds with no owner file" "down exited non-zero"
fi
if podman container exists "$CN_F" 2>/dev/null; then
    bad "F2: the container is gone" "container still exists"
else
    ok "F2: the container is gone"
    _ALL_CNAMES="${_ALL_CNAMES/ $CN_F/}"
fi

# ===========================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
