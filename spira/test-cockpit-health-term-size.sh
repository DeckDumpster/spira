#!/usr/bin/env bash
#
# test-cockpit-health-term-size.sh — health loop reads the pane's REAL size, not a
# hardcoded small default.
#
# THE DEFECT THIS REPRODUCES (found on the operator's live pane within a day of sp-llbmi
# landing). `term_size` shelled out to `stty size`. `std::process::Command::output()` gives
# that child process a NULL stdin — never this process's own tty — so `stty size` failed
# with "standard input: Inappropriate ioctl for device" on every single call, in every pane,
# unconditionally. `term_size` read that as "no reading" and fell to its last-resort
# default every tick. On the operator's real 81-row pane, `health loop` rendered the header
# and a fold marker (`▾N`) and nothing else, forever — not an occasional flicker, a
# permanent, 100%-reproducible five-row pane.
#
# THE FIX reads the terminal size with a direct ioctl(TIOCGWINSZ) on this PROCESS's own
# fd — no child process, so no stdin to lose.
#
# hermetic-ok: its own TMUX_TMPDIR server; touches no operator state
# tier: T1
# defect: sp-llbmi (found live, fixed same day)
# covers: cockpit/ops/src/health/term.rs cockpit/ops/src/health_main.rs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

command -v tmux >/dev/null 2>&1 || { echo "  SKIP  tmux is not on PATH"; exit 77; }
command -v health >/dev/null 2>&1 || bail "cannot find 'health' on PATH"

T="$(mktemp -d)"
RUN="$T/run"; mkdir -p "$RUN"
cleanup() { TMUX_TMPDIR="$T" tmux kill-server 2>/dev/null || true; rm -rf "$T"; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

echo "test-cockpit-health-term-size.sh"

# The exact live-pane reproduction: an 81-row, 168-column pane running `health loop`, given
# a moment to paint, then captured. Never the operator's own server — TMUX_TMPDIR scopes
# this to a private one, and $T is a throwaway directory no other test or process shares.
# SPIRA_RUN is a registered key (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare it via
# tl_config — `health` no longer reads the -e SPIRA_RUN below from its environment, but the
# new session's SPIRA_TOML is seeded from this process's, which tl_config writes into.
tl_config SPIRA_RUN="$RUN"
TMUX_TMPDIR="$T" tmux new-session -d -x 168 -y 81 -e "SPIRA_RUN=$RUN" "health loop"
sleep 3
OUT="$(TMUX_TMPDIR="$T" tmux capture-pane -p)"

nonblank="$(printf '%s\n' "$OUT" | grep -c '[^[:space:]]')"
# POSITIVE CONTROL: the header alone is ~4-6 lines. If the fold bug is back, this still
# passes trivially (a handful of non-blank lines) — the real assertions below are what
# distinguish "rendered everything" from "rendered five lines and stopped".
is "at least one line rendered at all" "1" "$([ "$nonblank" -gt 0 ] && echo 1 || echo 0)"

for label in ATTN SEND BEADS LAND LOCK GATE SUITES BOX MAIL OPS; do
    if printf '%s\n' "$OUT" | grep -qE "^ ?${label}( |$)"; then
        ok "the $label section rendered in an 81-row pane"
    else
        bad "the $label section rendered in an 81-row pane" \
            "not found; pane had $nonblank non-blank line(s):\n$OUT"
    fi
done

# THE SIGNATURE OF THE BUG ITSELF: a fold-drop marker on the header means the renderer
# believed the pane was shorter than it is. A correctly-read 81 rows must never need one
# here — OPS, the last standing line, printed above is direct proof the full frame fit.
if printf '%s\n' "$OUT" | head -1 | grep -q '▾'; then
    bad "no fold-drop marker on an 81-row pane" "header carries a ▾ marker: $(printf '%s\n' "$OUT" | head -1)"
else
    ok "no fold-drop marker on an 81-row pane"
fi

echo
tl_summary
