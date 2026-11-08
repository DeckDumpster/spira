#!/usr/bin/env bash
#
# test-perf-watch-units.sh — perf-watch's read contract: its log is a watchd row and the probe
# file names only well-formed hot paths. The units it once checked are gone: perf-watch and
# perf-happy-path are the scheduler's watch.perf and watch.perf_happy tasks, whose registration,
# cadence and fence are T1-T6 in spira-runtime/tests/it.
#
# tier: T0
# covers: spira/perf-probes spira/watchers
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "=== watcher row and probes ==="
want "the watchers file has a log row for the service's log" "perf-watch|log|@SPIRA_RUN@/perf-watch.log" "$(grep '^perf-watch|' "$HERE/watchers")"
probes="$(grep -v '^#' "$HERE/perf-probes" | grep .)"
is "the probe file names six hot paths" "6" "$(printf '%s\n' "$probes" | wc -l | tr -d ' ')"
want "the READY listing is probed" "lc-ready" "$probes"
bad_lines="$(printf '%s\n' "$probes" | grep -vP '^[a-z-]+\t\S+' )"
is "every probe is <name><TAB><command>" "" "$bad_lines"

tl_summary
