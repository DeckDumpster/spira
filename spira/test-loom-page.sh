#!/usr/bin/env bash
#
# test-loom-page.sh — runs loom/static/model.test.js under node's own test runner: the view
# model is derived in the page, not delivered pre-chewed.
#
#   ./test-loom-page.sh
#
# WHERE IT RUNS: the timed set, found by the `spira/test-*.sh` glob and run by `suites.sh`,
# because `gate-suites` does not name it. That is the right side of the line — it holds a
# derivation against a fixture, not a property of the pipeline, so a red here is a wrong
# answer worth a bead and a revert rather than a reason to stand between a branch and its
# landing (law-reversibility-outranks-coverage).
#
# 12 of the suite's 13 arms live in model.test.js now, run once under `node --test` instead of
# node spawned separately per assertion. The 13th — parsing what the tracker actually emits,
# over a throwaway bd database — is spira/test-cockpit-bd-contract.sh's "loom's model.js
# parses what the tracker actually emits" row: that suite already keeps one row per real-bd
# query shape, and a second bd-backed fixture here would duplicate its reason for existing.
#
# WHY node AND NOT A BROWSER. model.js is the half that touches no document and no network,
# which is exactly what lets the SHIPPED file be loaded under a bare JS runtime. A browser
# would test more and would also make this suite unrunnable on a machine that has no reason
# to have one; the rendering was verified separately, by eye and by a headless run, against
# the same fixture this asserts on.
#
# defect: sp-wok.2
# covers: loom/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
LOOM="$HERE/../loom/static"

NODE="$(command -v node || command -v nodejs || true)"
if [ -z "$NODE" ]; then
    # A LOUD SKIP, NEVER A SILENT PASS (gap #11: a missing subject must not read as green).
    # The page is JavaScript and there is no honest way to assert on it without a JS runtime;
    # exit 0 here would report the derivation as tested on every machine that cannot run it.
    echo "SKIP test-loom-page: no node on PATH — the view model cannot be exercised" >&2
    echo "     install node, or accept that loom/static/model.js is ungated on this box" >&2
    exit 77
fi

NODE_MAJOR="$("$NODE" -e 'console.log(process.versions.node.split(".")[0])' 2>/dev/null || echo 0)"
case "$NODE_MAJOR" in
    ''|*[!0-9]*) NODE_MAJOR=0 ;;
esac
if [ "$NODE_MAJOR" -lt 18 ]; then
    echo "SKIP test-loom-page: $($NODE -v 2>/dev/null) predates node --test (needs 18+)" >&2
    exit 77
fi

exec "$NODE" --test "$LOOM/model.test.js"
