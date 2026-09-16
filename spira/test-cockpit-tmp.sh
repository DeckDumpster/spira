#!/usr/bin/env bash
#
# test-cockpit-tmp.sh — the collector's temp file, seen surviving nothing.
#
#   ./test-cockpit-tmp.sh
#
# write_snapshot builds the snapshot in a temp and renames it, so probing — the slow part,
# tens of seconds — happens with a file already on disk. A kill landing in that window used
# to leave the temp behind, and spira-cockpit.service is Restart=always: 303 of them
# accumulated in .runtime/spira, four of those partial writes rather than empty. The cost is
# not the bytes. A directory that grows a file per unclean exit hides how often unclean exits
# happen, because nothing counts them.
#
# EVERY CASE CARRIES ITS POSITIVE CONTROL. "No temp remains" is the same observation as "the
# check ran against the wrong directory", as "the process died before it ever made one", and
# as "the glob was empty all along" — so each case first asserts the temp IS there, mid-probe,
# before killing anything. A leak check that has never seen a temp is a hypothesis
# (law-absence-needs-a-positive-control).
#
# If the temp never appears the suite SKIPS with 77 rather than passing: probe needs `bd` and
# `git` to get far enough to be interrupted, and a box where it cannot start is a box where
# this property is unobservable, not one where it holds.
#
# defect: sp-2yd
# covers: spira/cockpit.sh
# shellcheck disable=SC2034
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$HERE/cockpit.sh"
pass=0; fail=0

RUN="$(mktemp -d)"; trap 'rm -rf "$RUN"' EXIT

# temps -> how many .cockpit.* files are in the scratch run dir right now.
temps() { find "$RUN" -maxdepth 1 -name '.cockpit.*' | wc -l; }

ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s\n' "$1"; }

# A marker snapshot: an interrupted pass must not touch the one already published. The
# rename is what makes that true, and it is the other half of writing to a temp at all.
printf "SP_MARKER='before'\n" > "$RUN/cockpit.env"

echo "a killed pass leaves no temp behind"

# spawn -> start one collector pass in the background, with SIGINT at its default.
#
# A background job of a NON-INTERACTIVE shell inherits SIGINT and SIGQUIT as ignored, and a
# signal ignored on entry cannot be trapped or reset by bash. So `cockpit.sh once &` runs a
# collector whose INT trap was never installed: the first version of this suite sent SIGINT,
# the collector ignored it, ran to completion, and the case reported "no temp left" — true,
# and about a pass that was never interrupted. The exec shim restores the default disposition
# so the child models systemd and a terminal rather than this test's own shell.
#
# os.setpgrp() puts the child in its own process group so `kill -SIG -$p` reaches every
# probe subprocess. Without this, bash defers the signal trap until the running bd/python3
# child returns — probe queries the live database, which takes tens of seconds per pass,
# and the suite would block in `wait` for the full probe duration on every kill.
spawn() {
    SPIRA_RUN="$RUN" SPIRA_COCKPIT_FORCE=1 python3 -c '
import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.setpgrp()
os.execvp("bash", ["bash", sys.argv[1], "once"])
' "$COCKPIT" >/dev/null 2>&1 &
}

for sig in TERM INT HUP; do
    spawn
    p=$!

    # Positive control: wait for the temp to exist. It is created before probing starts, so
    # its absence after several seconds means the collector never reached the window under
    # test — not that the window is clean.
    seen=0
    for _ in $(seq 1 100); do
        if [ "$(temps)" -gt 0 ]; then seen=1; break; fi
        kill -0 "$p" 2>/dev/null || break
        sleep 0.1
    done
    if [ "$seen" -eq 0 ]; then
        kill -KILL "-$p" 2>/dev/null; wait "$p" 2>/dev/null
        echo "SKIP: cockpit.sh never created a temp — probe cannot start here (bd/git missing?)" >&2
        exit 77
    fi
    ok "$sig: temp present mid-probe (control)"

    # Kill the process group so probe's subprocesses also receive the signal: bash defers
    # its signal trap until the current foreground child returns, and probe's bd/python3
    # children have no trap — they exit immediately, unblocking bash's own trap.
    kill -"$sig" "-$p" 2>/dev/null
    wait "$p" 2>/dev/null; rc=$?

    # The signal is re-raised after cleanup rather than swallowed for a made-up status, so
    # the caller — systemd on a restart — still sees the death it sent. 128+signo.
    want=$((128 + $(kill -l "$sig")))
    [ "$rc" -eq "$want" ] && ok "$sig: died of the signal it was sent (rc=$rc)" \
                          || bad "$sig: expected rc $want, got $rc"

    n="$(temps)"
    [ "$n" -eq 0 ] && ok "$sig: no temp left" || bad "$sig: $n temp(s) left behind"
done

echo
echo "an interrupted pass does not disturb the published snapshot"
got="$(. "$RUN/cockpit.env" 2>/dev/null; printf '%s' "${SP_MARKER:-}")"
[ "$got" = "before" ] && ok "cockpit.env untouched by three killed passes" \
                      || bad "cockpit.env: marker is [$got], expected [before]"

echo
echo "sweep_stale_tmps clears pre-existing orphaned temps at startup"
# The sweep runs at the top of 'once' before probe is called, so once the orphans are gone
# the probe is still slow enough to safely interrupt.
touch "$RUN/.cockpit.99999" "$RUN/.cockpit.orphan"
# Use the same python3 shim as spawn so cockpit.sh runs in its own process group — kill
# -TERM -$sweep_pid then reaches probe's children too, not just bash.
SPIRA_RUN="$RUN" SPIRA_COCKPIT_FORCE=1 python3 -c '
import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.setpgrp()
os.execvp("bash", ["bash", sys.argv[1], "once"])
' "$COCKPIT" >/dev/null 2>&1 &
sweep_pid=$!
# Wait for the sweep to have run: the orphan count drops from 2 once they are removed.
# A fixed sleep is unreliable because sourcing lib.sh and conf.sh can take longer than any
# constant; the probe that follows sweep is slow enough that killing after the count drops is safe.
for _ in $(seq 1 200); do
    [ "$(temps)" -lt 1 ] && break
    kill -0 "$sweep_pid" 2>/dev/null || break
    sleep 0.1
done
kill -TERM "-$sweep_pid" 2>/dev/null; wait "$sweep_pid" 2>/dev/null || true
n="$(temps)"
[ "$n" -eq 0 ] && ok "orphaned temps removed by startup sweep" \
              || bad "$n orphaned temp(s) survived startup sweep"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
