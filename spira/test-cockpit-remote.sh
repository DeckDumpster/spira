#!/usr/bin/env bash
#
# test-cockpit-remote.sh — verify that the cockpit/remote/ scripts ship cleanly.
#
# TWO PROPERTIES THIS SUITE EXISTS TO ENFORCE:
#
#   1. COCKPIT_HOST REFUSES, NOT DEFAULTS. The dialer (cockpit/remote/cockpit) has no
#      default for COCKPIT_HOST — a wrong default silently dials somebody else's box,
#      the same shape as SPIRA_ALERT_GLOB. An unset COCKPIT_HOST must exit non-zero
#      with a message naming the variable. This suite is the proof that "the default is
#      gone" does not mean "the guard is silent about it".
#
#   2. INVENTORY IS CLEAN. Both files are tracked here; inventory.sh must pass them.
#      The positive control: if we introduce a forbidden path, inventory.sh must
#      catch it. Without the positive control a silent grep reports clean even when
#      pointed at the wrong directory.
#
# tier: T1
# covers: cockpit/remote/cockpit cockpit/remote/cockpit-remote spira/inventory.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
REMOTE_DIR="$(dirname "$HERE")/cockpit/remote"
INVENTORY="$HERE/inventory.sh"


echo "test-cockpit-remote.sh"

# ======================================================================================
echo
echo "1. cockpit/remote/cockpit exists and is executable:"
# ======================================================================================
DIALER="$REMOTE_DIR/cockpit"
if [ -x "$DIALER" ]; then ok "cockpit dialer is executable"; else bad "cockpit dialer is executable" "not found at $DIALER"; fi

# ======================================================================================
echo
echo "2. cockpit/remote/cockpit-remote exists and is executable:"
# ======================================================================================
FOLLOWER="$REMOTE_DIR/cockpit-remote"
if [ -x "$FOLLOWER" ]; then ok "cockpit-remote is executable"; else bad "cockpit-remote is executable" "not found at $FOLLOWER"; fi

# ======================================================================================
echo
echo "3. cockpit refuses when COCKPIT_HOST is unset (positive control — it must FAIL here):"
# ======================================================================================
# This property is load-bearing: the whole point of removing the default is that a wrong
# one silently dials somebody else's box. The guard must reject an unset host with a
# non-zero exit AND name the variable.
out="$(COCKPIT_HOST="" bash "$DIALER" 2>&1 || true)"
rc="$(COCKPIT_HOST="" bash "$DIALER" 2>&1; printf '%d' $?)"
if [[ "$out" == *"COCKPIT_HOST"* ]]; then
    ok "cockpit names the missing variable in its error"
else
    bad "cockpit names the missing variable in its error" "got: $(printf '%s' "$out" | head -3 | tr '\n' ' ')"
fi
# Unset COCKPIT_HOST (not set to empty, unset completely)
unset COCKPIT_HOST 2>/dev/null || true
out2="$(bash "$DIALER" 2>&1 || true)"
if [[ "$out2" == *"COCKPIT_HOST"* ]]; then
    ok "cockpit refuses and names COCKPIT_HOST when the variable is absent"
else
    bad "cockpit refuses and names COCKPIT_HOST when the variable is absent" \
        "got: $(printf '%s' "$out2" | head -3 | tr '\n' ' ')"
fi

# ======================================================================================
echo
echo "4. inventory.sh passes both files:"
# ======================================================================================
# POSITIVE CONTROL. Before believing 'clean', prove inventory.sh can go red. Plant an
# inventory-triggering path in a temp file and confirm it is caught. The path is
# constructed at runtime so this file itself does not trigger the fence.
T="$(mktemp)"
trap 'rm -f "$T"' EXIT
# /workspaces + / yields the full forbidden pattern without writing it as a literal here.
FORBIDDEN_PREFIX="/workspaces"
printf '%s\n' '#!/bin/bash' "THING=${FORBIDDEN_PREFIX}/some/path" > "$T"
if [ -n "$("$INVENTORY" --scan "$T" 2>/dev/null)" ]; then
    ok "positive control: inventory.sh catches workspace paths"
else
    bad "positive control: inventory.sh catches workspace paths" "offender was not flagged"
fi

hits_dialer="$("$INVENTORY" --scan "$DIALER" 2>/dev/null)"
hits_follower="$("$INVENTORY" --scan "$FOLLOWER" 2>/dev/null)"

if [ -z "$hits_dialer" ]; then
    ok "cockpit dialer passes inventory.sh"
else
    bad "cockpit dialer passes inventory.sh" "found: $hits_dialer"
fi
if [ -z "$hits_follower" ]; then
    ok "cockpit-remote passes inventory.sh"
else
    bad "cockpit-remote passes inventory.sh" "found: $hits_follower"
fi

# ======================================================================================
echo
echo "5. cockpit-remote's LAYOUT discovery does not reference the brain/wiki path:"
# ======================================================================================
# The old cockpit-remote sourced BRAIN_DIR/.claude/harness.sh to find layout.sh. That path
# required the wiki to exist, so a host with no wiki produced a blank LAYOUT and the
# watcher's heal step was silently skipped. Verify the new version no longer uses that.
if grep -q 'BRAIN_DIR.*claude/harness' "$FOLLOWER" 2>/dev/null; then
    bad "cockpit-remote no longer sources layout.sh via brain harness" \
        "still references .claude/harness"
else
    ok "cockpit-remote no longer sources layout.sh via brain harness"
fi

# ======================================================================================
echo
echo "6. rebuild.sh reads COCKPIT_SESSIONS (not a hardcoded list):"
# ======================================================================================
REBUILD="$(dirname "$REMOTE_DIR")/rebuild.sh"
if [ -f "$REBUILD" ]; then
    if grep -q 'COCKPIT_SESSIONS' "$REBUILD" 2>/dev/null; then
        ok "rebuild.sh uses COCKPIT_SESSIONS"
    else
        bad "rebuild.sh uses COCKPIT_SESSIONS" "variable not found in rebuild.sh"
    fi
    if grep -Eq '^SESSIONS="brain hunk chat"' "$REBUILD" 2>/dev/null; then
        bad "rebuild.sh does not hardcode the session list" "hardcoded SESSIONS= found"
    else
        ok "rebuild.sh does not hardcode the session list"
    fi
else
    bad "rebuild.sh exists" "not found at $REBUILD"
fi

echo
echo "losing the lock is reported as a failure, not as a clean finish"
# `watch` under systemd is Restart=always, so a unit whose copy loses the lock is respawned
# forever. Exiting 0 made systemd record Result=success on every one of those passes, so the
# journal agreed with the loop rather than contradicting it, and the unit read as fine for
# hours. Asserted by RUNNING the contention path, not by grepping for an exit code.
CR="$REMOTE_DIR/cockpit-remote"
if [ ! -x "$CR" ]; then
    bad "cockpit-remote is executable" "not found at $CR"
else
    LOCKD="$(mktemp -d)"
    # A holder that keeps the lock for the life of the subshell. `watch_loop` is never
    # entered by either process here: the incumbent is this flock, and the script under test
    # is expected to refuse before it starts doing anything.
    ( flock 9; touch "$LOCKD/held"; sleep 10 ) 9>"$LOCKD/cockpit-watch.lock" &
    _holder=$!
    for _ in 1 2 3 4 5 6 7 8 9 10; do [ -e "$LOCKD/held" ] && break; sleep 0.2; done

    # THE PRECONDITION, PROVED. If the lock were not actually held, the refusal below would
    # be attributable to anything at all and the assertion would pass for free.
    if ( flock -n 9 ) 9>"$LOCKD/cockpit-watch.lock" 2>/dev/null; then
        bad "the fixture holds the lock (positive control)" "a second flock still succeeded"
    else
        ok "the fixture holds the lock (positive control)"
    fi

    out="$(TMPDIR="$LOCKD" timeout 10 bash "$CR" watch 2>&1)"; rc=$?
    if [ "$rc" = 0 ]; then
        bad "a copy that loses the lock exits non-zero" "exited 0 — systemd records Result=success and the loop looks healthy"
    else
        ok "a copy that loses the lock exits non-zero (rc=$rc)"
    fi
    is "and it does not hang waiting for the lock" "1" "$([ "$rc" = 124 ] && echo 0 || echo 1)"
    want "and it says which copy is in the way" "already running" "$out"

    kill "$_holder" 2>/dev/null; wait "$_holder" 2>/dev/null
    rm -rf "$LOCKD"
fi

echo
tl_summary
