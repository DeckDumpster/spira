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

# ---------------------------------------------------------------------------
echo
echo "E ACCEPTANCE: past the usage threshold the biggest spill goes, a live holder or not"
# ---------------------------------------------------------------------------
PDIR="$TMP/pressure"
mkdir -p "$PDIR"
head -c 3000000 /dev/zero > "$PDIR/spill_big"
head -c 1000000 /dev/zero > "$PDIR/spill_small"
used_pct() { local k; k=$(du -sk "$PDIR" | cut -f1); echo $(( k * 100 / 8000 )); }   # 8000 KiB "tmpfs"
exec 8<>"$PDIR/spill_big"
out_e="$(_dolt_tmp_prune_pressure "$PDIR" 90 used_pct)"
[ -e "$PDIR/spill_big" ] && [ -e "$PDIR/spill_small" ] \
    && ok "E-: below the threshold nothing is touched — the check ran, it did not just delete" \
    || bad "E-: below threshold nothing is touched" "removed: $out_e"
head -c 3000000 /dev/zero > "$PDIR/spill_big2"
head -c 3000000 /dev/zero >> "$PDIR/spill_big"
out_e="$(_dolt_tmp_prune_pressure "$PDIR" 60 used_pct)"
[ ! -e "$PDIR/spill_big" ] \
    && ok "E: the largest entry is removed although this process holds it open" \
    || bad "E: largest entry removed under pressure" "still there; output: $out_e"
[ "$(stat -L -c %s /proc/$$/fd/8)" = 0 ] \
    && ok "E: the held spill is truncated, so its blocks are freed before the holder lets go" \
    || bad "E: held spill truncated" "size $(stat -L -c %s /proc/$$/fd/8)"
exec 8<&-
[[ "$out_e" == *"spill_big"* ]] \
    && ok "E: removal is reported" || bad "E: removal is reported" "got: $out_e"
[ "$(used_pct)" -lt 60 ] \
    && ok "E: it stops once usage is back under the threshold" \
    || bad "E: stops under threshold" "usage $(used_pct)%"
[ -e "$PDIR/spill_small" ] \
    && ok "E: smaller entries survive once the pressure is relieved" \
    || bad "E: smaller entries survive" "spill_small removed"

echo
echo "F: the real usage reader agrees with df"
want="$(df --output=pcent "$TMP" | tail -n1 | tr -dc '0-9')"
[ -n "$want" ] && [ "$(_dolt_tmp_used_pct "$TMP")" = "$want" ] \
    && ok "F: _dolt_tmp_used_pct reads the filesystem's percent" \
    || bad "F: _dolt_tmp_used_pct matches df" "want $want got $(_dolt_tmp_used_pct "$TMP")"
EDIR="$TMP/empty"; mkdir -p "$EDIR"
always_full() { echo 99; }
[ -z "$(_dolt_tmp_prune_pressure "$EDIR" 10 always_full)" ] \
    && ok "F: a full filesystem with an empty spool ends the loop instead of spinning" \
    || bad "F: empty spool ends the loop" "produced output"

echo
tl_summary
