#!/usr/bin/env bash
# covers: spira/world.sh
#
# `world.sh stop --hard` stops the long-running watcher SERVICES; `world.sh start` must
# start them again. For a long time it did not -- it started only the timers -- and the
# watchers stayed dead until somebody noticed by hand.
#
# WHY THIS IS THE EXPENSIVE ONE. The answers watcher is the leg that carries the operator's
# verdict back out of the cockpit. Dead, the operator answers, the bead closes, and no
# session ever learns of it: they believe they have replied and the work does not move.
# Nine answers sat undelivered for eight hours (law-answers-need-a-delivery-path).
#
# NOTHING HERE TOUCHES systemd. The suite reads world.sh, because driving the real user
# manager would stop watchers under whoever is attached to this box.
#
# MATCHERS READ CODE, NOT PROSE (law-a-matcher-reads-code-not-prose): world.sh explains
# this mechanism at length in the comment right above it, so a whole-file grep matches the
# explanation as readily as the mechanism.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
WORLD="$HERE/world.sh"

CODE="$(grep -vE '^[[:space:]]*#' "$WORLD")"
# The `start` branch alone. A match anywhere else in the file -- `stop --hard` already
# enumerates the same units -- would pass while `start` still did nothing, which is
# precisely the bug.
START_BLOCK="$(printf '%s' "$CODE" | awk '/^start\)/{f=1} f{print} f&&/^    ;;/{exit}')"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
inblock() { grep -qE "$1" <<< "$START_BLOCK"; }

echo "test-world-start-revives-watchers.sh"
echo

# Guard the harness of the suite itself: if the awk above ever stops finding the branch,
# every assertion below would pass vacuously against an empty string.
if [ "$(printf '%s' "$START_BLOCK" | wc -l)" -ge 5 ]; then
    ok "the start branch was located (positive control)"
else
    bad "the start branch was located (positive control)" "extracted $(printf '%s' "$START_BLOCK" | wc -l) lines; every assertion below would be vacuous"
fi

echo
echo "start is symmetric with stop --hard:"
if inblock 'spira-watch'; then
    ok "start acts on the watcher units"
else
    bad "start acts on the watcher units" "start touches only timers; stop --hard leaves the watchers dead and nothing revives them"
fi
if inblock 'list-unit-files|list-units'; then
    ok "the units are queried from systemd, not hand-listed"
else
    bad "the units are queried from systemd, not hand-listed" "a hand-written list is one a watcher added later escapes silently"
fi
if inblock 'SPIRA_INSTANCE'; then
    ok "the instance-qualified unit names are matched"
else
    bad "the instance-qualified unit names are matched" "after the per-instance migration the bare names match nothing"
fi

echo
echo "it distinguishes the two kinds of watcher by what they ARE:"
# The timer-driven watchers are Type=oneshot and their timers are started separately;
# firing the service directly would run one pass out of band. Excluding them BY NAME
# would rebuild the hand-written-list defect this block exists to fix.
if inblock 'Type.*oneshot|oneshot'; then
    ok "oneshot units are skipped by type"
else
    bad "oneshot units are skipped by type" "no Type check; either the oneshots fire out of band or they are excluded by a name list"
fi
if inblock 'watch-refresh|watch-notify'; then
    bad "no watcher is excluded by name" "names a specific watcher; a watcher added later escapes the same way this bug did"
else
    ok "no watcher is excluded by name"
fi

echo
echo "it is idempotent, because start is run on a healthy world too:"
if inblock 'is-active'; then
    ok "an already-running watcher is left alone"
else
    bad "an already-running watcher is left alone" "restarting a healthy watcher drops whatever it was mid-way through"
fi

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
