#!/usr/bin/env bash
#
# test-strand-throttle.sh — strand.sh suppresses 'starved' while the admission throttle
#   is engaged, and refuses to guess when the throttle stamp cannot be read.
#
#   ./test-strand-throttle.sh
#
# WHY THIS EXISTS. sp-lsh22, sp-fmhwi (2026-09-24): the [spira,plan] queue escalated as
# "stranded (starved)" while CHECK7's own log line in the same pass read "throttle active
# (... depth=12 since_land=4m) — task pool held at 0". Ryan's verdict: "nothing was
# stranded; the queue depth was too high to allocate work to aeons" — the detector must
# check the throttle before calling withheld aeons a strand.
#
# FOUR CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — THROTTLE_STATE=open, ready beads, no live aeon → classifier
#      DOES produce "starved". Proves the check can fire before we trust its silence.
#
#   1. THROTTLE SHUT — THROTTLE_STATE=shut, same fixture → no "starved" in output;
#      "throttled" IS emitted as an info row naming depth and release-at (never escalate).
#
#   2. THROTTLE UNREADABLE — THROTTLE_STATE=unreadable → neither "starved" nor "throttled"
#      is emitted; a distinct "throttle-unreadable" row is (law-a-control-that-cannot-
#      check-must-refuse: it does not assume open, and it does not assume shut).
#
#   3. throttle_state() itself — sourced from strand.sh against a real stamp file: absent
#      stamp reads "open", a readable stamp reads "shut" with its depth, and a stamp made
#      unreadable (chmod 000) reads "unreadable" rather than silently falling back to
#      either of the other two.
#
# PRE-FIX FAILURE (run against unfixed strand-classify.py / strand.sh):
#
#   FAIL  throttle shut: throttled IS emitted: wanted [throttled] in [starved\t-\tescalate\t...]
#   FAIL  throttle shut: starved NOT emitted: did not want [starved] in [starved\t-\tescalate\t...]
#   FAIL  throttle unreadable: throttle-unreadable IS emitted: wanted [throttle-unreadable] in []
#   FAIL  throttle unreadable: starved NOT emitted: did not want [starved] in [starved\t-\t...]
#   FAIL  throttle_state: unreadable stamp reads 'unreadable': wanted [unreadable] in [open ]
#
# defect: sp-0ua6w
# tier: T1
# covers: strand/src/*
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# THE STRAND IS A BINARY (strand.sh and strand-classify.py are gone). Cases 0-2 drove the
# python classifier with THROTTLE_STATE; they are `cargo test -p strand` now (classify::tests,
# the starved/throttled/throttle-unreadable variants). Case 3 runs the binary's own reader.
STRAND_BIN="${SPIRA_STRAND_BIN:-$(SPIRA_HOME="$HERE" bash -c '. "$1/conf.sh" >/dev/null 2>&1; spira_bin strand 2>/dev/null' _ "$HERE")}"

echo "test-strand-throttle.sh"

# ======================================================================================
echo
echo "case 3 — 'strand throttle-state' against a real stamp file:"
# ======================================================================================
mkdir -p "$TMP/run"
out="$(SPIRA_RUN="$TMP/run" "$STRAND_BIN" throttle-state 2>&1)"
want "throttle_state: no stamp reads open" $'open\t' "$out"

_stamp="$TMP/run/queue-throttled"
printf 'since=2026-09-24T18:01:47Z depth=12 since_land=4m\n' > "$_stamp"
out="$(SPIRA_RUN="$TMP/run" "$STRAND_BIN" throttle-state 2>&1)"
want "throttle_state: readable stamp reads shut" $'shut\tdepth 12' "$out"

chmod 000 "$_stamp"
out="$(SPIRA_RUN="$TMP/run" "$STRAND_BIN" throttle-state 2>&1)"
chmod 644 "$_stamp"
want "throttle_state: unreadable stamp reads unreadable" "unreadable" "$out"
tl_summary
