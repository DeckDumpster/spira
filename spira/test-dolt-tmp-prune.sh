#!/usr/bin/env bash
# test-dolt-tmp-prune.sh — dolt-tmp-prune.sh: the sweep that bounds the live
# dolt-beads server's own temp spool (sp-n1l7y). Nothing removed a spill file there
# before; 1,211 files and 10GB accumulated in four days of restart-limit crashes.
#
# WHAT THIS TESTS
#   A. a stale, unheld entry is removed, and reported.
#   B. a fresh entry survives even though nothing holds it open.
#   C. an entry a REAL process (this shell) holds a file descriptor open under
#      survives past the window — the safety rule the incident used by hand — and
#      is prunable once the descriptor closes. Exercises _dolt_tmp_held_fds against
#      this process's own /proc/$$/fd, not a hand-written model of it.
#   D. an empty pid holds nothing — the dead-server case, which must prune freely.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   B reuses A's exact fixture shape minus the one property under test (age), so its
#   survival proves the age check ran rather than the sweep finding nothing to do.
#
# tier: T1
# covers: spira/dolt-tmp-prune.sh systemd/dolt-tmp-prune.service systemd/dolt-tmp-prune.timer
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

. "$HERE/testlib.sh"
. "$HERE/dolt-tmp-prune.sh"

echo "test-dolt-tmp-prune.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
DIR="$TMP/tmp"
mkdir -p "$DIR"

WINDOW=3600

mk_entry() {  # mk_entry <name> <age-seconds>
    local f="$DIR/$1"
    : > "$f"
    touch -d "$2 seconds ago" "$f"
}

# ---------------------------------------------------------------------------
echo
echo "A: a stale, unheld entry is removed"
# ---------------------------------------------------------------------------
mk_entry stale_a $((WINDOW + 100))
out_a="$(_dolt_tmp_prune_stale "$DIR" "$WINDOW" "")"
[ ! -e "$DIR/stale_a" ] \
    && ok "A: stale entry is removed" \
    || bad "A: stale entry is removed" "file still exists"
[[ "$out_a" == *"stale_a"* ]] \
    && ok "A: removal is reported on stdout" \
    || bad "A: removal is reported on stdout" "got: $out_a"

# ---------------------------------------------------------------------------
echo
echo "B POSITIVE CONTROL: a fresh entry survives"
# ---------------------------------------------------------------------------
mk_entry fresh_b 10
_dolt_tmp_prune_stale "$DIR" "$WINDOW" "" >/dev/null
[ -e "$DIR/fresh_b" ] \
    && ok "B: fresh entry survives — the age check is read, not skipped" \
    || bad "B: fresh entry survives" "file was removed"

# ---------------------------------------------------------------------------
echo
echo "C ACCEPTANCE: an entry a real process holds a fd open under survives past the window"
# ---------------------------------------------------------------------------
mk_entry held_c $((WINDOW + 100))
exec 9<>"$DIR/held_c"
held="$(_dolt_tmp_held_fds "$$")"
[[ "$held" == *"$DIR/held_c"* ]] \
    && ok "C: _dolt_tmp_held_fds names the file this real process has open" \
    || bad "C: _dolt_tmp_held_fds names the file this real process has open" "got: $held"
out_c="$(_dolt_tmp_prune_stale "$DIR" "$WINDOW" "$held")"
[ -e "$DIR/held_c" ] \
    && ok "C: held entry survives past the window" \
    || bad "C: held entry survives" "file was removed; sweep output: $out_c"
exec 9<&-
_dolt_tmp_prune_stale "$DIR" "$WINDOW" "" >/dev/null
[ ! -e "$DIR/held_c" ] \
    && ok "C+: once the descriptor closes, the next pass prunes it" \
    || bad "C+: descriptor release lets the entry be pruned" "file still exists"

# ---------------------------------------------------------------------------
echo
echo "D: an empty pid — the dead-server case — holds nothing"
# ---------------------------------------------------------------------------
empty_held="$(_dolt_tmp_held_fds "")"
[ -z "$empty_held" ] \
    && ok "D: an empty pid returns nothing held" \
    || bad "D: an empty pid returns nothing held" "got: $empty_held"

echo
tl_summary
