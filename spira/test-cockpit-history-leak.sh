#!/usr/bin/env bash
#
# test-cockpit-history-leak.sh — append_history's load-bearing subshell (cockpit.sh's own
# comment at the top of the function): sourcing the snapshot to read its keys must not leak
# them into the calling process, or a pass whose probe failed (leaving that key absent from
# cockpit.env) would silently reuse the PREVIOUS pass's value instead of writing '?'.
#
# This matters only for a long-lived process: `loop` mode calls write_snapshot — and so
# append_history — repeatedly from the SAME bash process (`while :; do write_snapshot;
# sleep "$INTERVAL"; done`), so any leaked variable survives to the next iteration. A fresh
# `cockpit.sh history` invocation would not show it: a new process has no prior value to
# leak. The driver below reproduces the same-process, two-pass shape.
#
# Before this suite, gap #7 of docs/test-plan/cockpit-observability.md: only
# test-beads-sparklines.sh touched this function, and only to assert an unrelated Python
# block does not reference "history" — nothing exercised the leak the comment describes.
#
# tier: T1
# covers: spira/cockpit.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$HERE/cockpit.sh"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()  { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
RUN="$TMP/run"; mkdir -p "$RUN"
SNAP="$RUN/cockpit.env"
HIST="$RUN/cockpit-history.csv"

# The real function, extracted from cockpit.sh at runtime (not copied) so the suite tracks
# the code it claims to cover — the same technique test-cockpit-collector-watchdog.sh uses.
FUNC_BODY=$(awk '/^append_history\(\)/{p=1} p{print} p && /^\}$/{exit}' "$COCKPIT")
[ -n "$FUNC_BODY" ] || { echo "SKIP: append_history not found in cockpit.sh" >&2; exit 1; }

HIST_COLS="ts,tok_win,tok_aeon_win,tok_sess_win,tok_aeon_turns,tok_sess_turns,ctx_now,ratelim_5h,ratelim_7d"

# Nth data row's tok_win column (row 1 = first line after the header).
tok_win_of_row() { awk -F, -v r="$2" 'NR==r+1{print $2}' "$1"; }

# =============================================================================
# POSITIVE CONTROL: a hand-rolled variant that sources the snapshot DIRECTLY into the
# calling shell (no subshell) is exactly the bug the real function's comment warns against.
# If this suite's method could not catch that, a silent leak in the real function would pass
# unnoticed too
# (law-a-check-that-finds-nothing-must-first-prove-it-could-have-found-something).
# =============================================================================
echo "positive control: sourcing the snapshot without a subshell leaks the prior value"

LEAKY="$TMP/leaky.out"
echo "SP_TOK_WIN=100" > "$SNAP"
bash <<DRIVER > "$LEAKY"
set -uo pipefail
SNAP="$SNAP"
leaky_read() {
    set +u
    . "\$SNAP" 2>/dev/null
    echo "\${SP_TOK_WIN:-?}"
}
leaky_read
: > "\$SNAP"
leaky_read
DRIVER
leaky_pass1="$(sed -n '1p' "$LEAKY")"
leaky_pass2="$(sed -n '2p' "$LEAKY")"
is "positive control pass 1: reads 100" "100" "$leaky_pass1"
is "positive control pass 2: LEAKS 100 (the bug the subshell prevents)" "100" "$leaky_pass2"

# =============================================================================
# THE REAL FUNCTION: same two-pass shape, run in one bash process the way `loop` does.
# Pass 1 has SP_TOK_WIN set; pass 2's snapshot omits it (the probe failed). The real
# function's subshell must render '?' on pass 2, not the value pass 1 measured.
# =============================================================================
echo ""
echo "real append_history: an absent key on the next pass renders ?, not the last value"

echo "SP_TOK_WIN=100" > "$SNAP"
bash <<DRIVER
set -uo pipefail
HIST="$HIST"
HIST_COLS="$HIST_COLS"
SNAP="$SNAP"
HISTORY_MAX=20160
$FUNC_BODY
append_history
: > "\$SNAP"
append_history
DRIVER

row1="$(tok_win_of_row "$HIST" 1)"
row2="$(tok_win_of_row "$HIST" 2)"
is "pass 1: tok_win=100" "100" "$row1"
is "pass 2 (probe failed): tok_win=?, not the leaked 100" "?" "$row2"

printf '\n  %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
