#!/usr/bin/env bash
#
# test-landing-prior-pass-t1.sh — prior_pass_suites() (landing-lib.sh), the pure extraction
# a cert-gate-red reopen note depends on to name the suite set a branch's own recorded PASS
# covered next to the suite certification just failed on. Every row is a function call over
# a literal gate-run.sh --status transcript — no git, no gate-run.sh process, no landing
# pass (sp-0pk2x: 73 of 76 cert-gate-red reopens carried no record of what a prior PASS had
# covered, because nothing extracted it).
#
# host-reason: sources landing-lib.sh only; no database, no systemd, no git
# tier: T1
# covers: spira/landing-lib.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/landing-lib.sh"

echo "test-landing-prior-pass-t1.sh"

# 1. A REAL PASS TRANSCRIPT, as gate-run.sh --status prints it, names the suite set.
out="$(printf '%s\n' \
    'gate-run: PASSED spira/sp-abc123 in spira after 42s' \
    'gate-run: key deadbeef cafefeed' \
    'gate-run: gate PASS covered suites: test-a.sh,test-b.sh' \
    'gate: VERDICT=PASS reason=pass branch=spira/sp-abc123 repo=spira suite=-')"
is "prior_pass_suites(): extracts the covered suite list from a PASS transcript" \
    "test-a.sh,test-b.sh" "$(prior_pass_suites "$out")"

# POSITIVE CONTROL: change the covered-suites line and the extraction moves with it —
# proves the sed pattern is reading the line, not returning a fixed string.
out="$(printf '%s\n' \
    'gate-run: PASSED spira/sp-abc123 in spira after 5s' \
    'gate-run: key deadbeef cafefeed' \
    'gate-run: gate PASS covered suites: test-only.sh')"
is "prior_pass_suites(): moves with the transcript, not a fixed string" \
    "test-only.sh" "$(prior_pass_suites "$out")"

# 2. NO SUCH LINE (an older gate-run.sh, or a transcript from a non-PASS status) -> empty.
out="$(printf '%s\n' \
    'gate-run: FAILED spira/sp-abc123 in spira after 5s (gate.sh exit 1)' \
    'gate-run: key deadbeef cafefeed')"
is "prior_pass_suites(): a FAIL transcript names nothing" "" "$(prior_pass_suites "$out")"

# 3. EMPTY INPUT (gate-run.sh --status exited non-zero, caller passed nothing) -> empty.
is "prior_pass_suites(): empty input names nothing" "" "$(prior_pass_suites "")"

# 4. THE MARKER LINE ITSELF, WITH "-" (a real PASS whose ran_suites() found no suite
#    filenames at all — e.g. a syntax-only gate) -> "-", not empty, so a caller can tell
#    "covered nothing identifiable" apart from "no PASS record exists".
out="$(printf '%s\n' 'gate-run: gate PASS covered suites: -')"
is "prior_pass_suites(): a PASS with no suites names '-'" "-" "$(prior_pass_suites "$out")"

tl_summary
