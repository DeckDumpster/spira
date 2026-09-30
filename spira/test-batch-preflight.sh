#!/usr/bin/env bash
#
# test-batch-preflight.sh — the fast local batch pre-flight (sp-mb92t).
#
# Per Ryan, 2026-09-24: the pre-flight "CANNOT take more than 4 minutes locally; if it does,
# we need to start evicting tests." What that needs to be true:
#
#   1. (retired, sp-wx2tw) fast-suites.sh and the selector's fast filter are gone: the Rust
#      gate runs the gate command under env -i, which never passed SPIRA_GATE_FAST_MAX_SECS
#      through, so the filter had not run since sp-0tpcs. suite-select/DESIGN.md names it.
#   2. _pf_run stops at the wall, returns 124, and kills the WHOLE process group — a test
#      container outliving the wall is exactly the failure the wall exists to prevent.
#      A command that finishes inside the wall keeps its own exit status.
#   3. _pf_left counts down to a shared deadline and never goes negative.
#
# covers: spira/batch.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# --- 2 and 3. the wall -----------------------------------------------------------------------
# Lift just the pre-flight helpers out of batch.sh (it runs main when sourced).
helpers="$(sed -n '/^_pf_run() {/,/^_batch_open_file() {/p' "$HERE/batch.sh" | sed '$d')"
[ -n "$helpers" ] || bail "could not extract the _pf_ helpers from batch.sh"
eval "$helpers"

start=$SECONDS
_pf_run 2 bash -c 'sleep 30 & echo $! > "'"$T"'/grandchild"; sleep 30'; rc=$?
took=$(( SECONDS - start ))
wantrc "a command past the wall returns 124"  124 "$rc"
[ "$took" -le 20 ] && ok "the wall stops it promptly (${took}s)" || bad "the wall stops it promptly: took ${took}s"
gc="$(cat "$T/grandchild" 2>/dev/null)"
sleep 1
if [ -n "$gc" ] && ! kill -0 "$gc" 2>/dev/null; then ok "the whole process group is killed, grandchild included"
else bad "the whole process group is killed: grandchild ${gc:-?} still alive"; kill "$gc" 2>/dev/null; fi

_pf_run 10 bash -c 'exit 3'; rc=$?
wantrc "a command inside the wall keeps its status" 3 "$rc"

# The watchdog must not outlive a command that finished: its sleep inherits every open
# descriptor, including batch.sh's queue lock. Hold a lock on fd 9, finish fast, and the
# lock must be free the moment _pf_run returns.
exec 9>"$T/lock"; flock -n 9 || bail "could not take the fixture lock"
_pf_run 30 true
exec 9>&-
flock -n "$T/lock" true && ok "no watchdog child holds an inherited lock after a fast finish" \
    || bad "a watchdog child still holds the inherited lock after _pf_run returned"
_pf_run 0 true; rc=$?
wantrc "no time left is the wall, not a run"  124 "$rc"

_PF_DEADLINE=$(( $(date +%s) + 30 ))
l="$(_pf_left)"; [ "$l" -ge 28 ] && [ "$l" -le 30 ] && ok "_pf_left counts down to the deadline ($l)" || bad "_pf_left counts down: got $l"
_PF_DEADLINE=$(( $(date +%s) - 5 ))
is     "_pf_left never goes negative"         "0" "$(_pf_left)"

tl_summary
