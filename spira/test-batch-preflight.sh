#!/usr/bin/env bash
#
# test-batch-preflight.sh — the fast local batch pre-flight (sp-mb92t).
#
# Per Ryan, 2026-09-24: the pre-flight "CANNOT take more than 4 minutes locally; if it does,
# we need to start evicting tests." What that needs to be true:
#
#   1. fast-suites.sh keeps a suite whose median is at or under the cap, DROPS one over it
#      and names it with its median, KEEPS a suite with no history, keeps everything (and
#      says so) when there is no timing log, and refuses a non-numeric cap. It reads the
#      suite list from stdin: its first version passed its program as a heredoc, which took
#      stdin, and silently returned nothing for a 141-suite selection.
#   2. _pf_run stops at the wall, returns 124, and kills the WHOLE process group — a test
#      container outliving the wall is exactly the failure the wall exists to prevent.
#      A command that finishes inside the wall keeps its own exit status.
#   3. _pf_left counts down to a shared deadline and never goes negative.
#   4. gate-touched.sh applies the fast filter only when SPIRA_GATE_FAST_MAX_SECS is set.
#
# covers: spira/fast-suites.sh spira/batch.sh spira/gate-touched.sh spira/tsd-query.sh
# timeout: 120
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM

# --- 1. fast-suites.sh -----------------------------------------------------------------------
# A hand-written run/tsd/suite-timing.jsonl fixture — no tsd-write build needed, just rows in
# the shape it produces (test-tsd.sh covers the write path). suite-medians reads it straight.
RUN="$T/run"; mkdir -p "$RUN/tsd"
FAM="$RUN/tsd/suite-timing.jsonl"
: > "$FAM"
_row() { printf '{"ts":"2026-09-25T00:00:%02dZ","host":"h1","family":"suite-timing","suite":"%s","wall_secs":%s}\n' "$1" "$2" "$3" >> "$FAM"; }
i=0
for w in 10 10 10;  do _row $((i+=1)) fast.sh "$w"; done
for w in 300 300 300; do _row $((i+=1)) slow.sh "$w"; done
for w in 60 60 60;  do _row $((i+=1)) edge.sh "$w"; done

DUCKDB_BIN="$(command -v duckdb 2>/dev/null || true)"
if [ -z "$DUCKDB_BIN" ]; then
    echo "SKIP section 1: duckdb not found — fast-suites.sh needs tsd-query.sh's query layer"
else
    fs() { SPIRA_HOME="$T" SPIRA_RUN="$RUN" SPIRA_DB="$T/db" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
               bash "$HERE/fast-suites.sh" "$@"; }
    mkdir -p "$T/db"

    out="$(printf 'fast.sh\nslow.sh\nedge.sh\nnew.sh\n' | fs --max-secs 60 2>"$T/err")"
    want   "keeps a suite under the cap"          "fast.sh" "$out"
    want   "keeps a suite exactly at the cap"     "edge.sh" "$out"
    want   "keeps a suite with no history"        "new.sh"  "$out"
    nowant "drops a suite over the cap"           "slow.sh" "$out"
    want   "names the dropped suite and median"   "dropped slow.sh (median 300s over 3 runs > 60s)" "$(cat "$T/err")"
    is     "reads all four suites from stdin"     "3" "$(printf '%s\n' "$out" | grep -c .)"

    EMPTY_RUN="$T/run-empty"; mkdir -p "$EMPTY_RUN"
    out="$(SPIRA_HOME="$T" SPIRA_RUN="$EMPTY_RUN" SPIRA_DB="$T/db" SPIRA_REPO="$HERE/.." SPIRA_CONF=/nonexistent \
               bash "$HERE/fast-suites.sh" --max-secs 60 <<<$'a.sh\nb.sh' 2>"$T/err")"
    is     "no timing history keeps everything"   "2" "$(printf '%s\n' "$out" | grep -c .)"
    want   "no timing history says so"            "no suite-timing history" "$(cat "$T/err")"

    fs --max-secs abc </dev/null >/dev/null 2>&1; rc=$?
    wantrc "a non-numeric cap is refused"         2 "$rc"
fi

# --- 2 and 3. the wall -----------------------------------------------------------------------
# Lift just the pre-flight helpers out of batch.sh (it runs main when sourced).
helpers="$(sed -n '/^_pf_run() {/,/^_lg_red_suites() {/p' "$HERE/batch.sh" | sed '$d')"
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

# --- 4. gate-touched applies the filter only when asked --------------------------------------
want   "gate-touched runs fast-suites under SPIRA_GATE_FAST_MAX_SECS" \
       'if [ -n "${SPIRA_GATE_FAST_MAX_SECS:-}" ]; then' "$(cat "$HERE/gate-touched.sh")"
want   "batch.sh's pre-flight gate sets it"   'SPIRA_GATE_FAST_MAX_SECS="${SPIRA_PREFLIGHT_SUITE_MAX_SECS:-60}"' "$(cat "$HERE/batch.sh")"

tl_summary
