#!/usr/bin/env bash
#
# test-thrash.sh — the deliverable-progress probe was dwarf_fuse_minutes (lib.sh);
# RETIRED (wave 4.34, sp-27d3d): ported to dwarf::trace::dwarf_fuse_minutes, with dwarf's own
# RealBeat::fuse() and cockpit-collect's probe both calling it in-process now. Parts 1-4b
# (no worktree → ?, a fresh write → 0, a stale write → a number, live-gate suppression and
# a gate that just finished resetting the fuse) moved to dwarf/src/trace.rs's own tests
# (dwarf_fuse_minutes_*), which drive the same fixtures (a real git worktree, a renamed
# `sleep` standing in for a live gate, aged files) without a bash sourcing shim in between.
#
# The wall detects when a dwarf's turns advance but its deliverable (commits ahead
# of the base ref or file writes in the worktree) has not moved for SPIRA_THRASH_MINUTES.
# A running gate suppresses the fuse so a correct mid-review dwarf is never tripped.
#
# The trip decision itself (both the fuse AND the session's own age must clear the wall)
# is hb_tick's table, tested in test-dwarf-lease.sh. The teardown behaviour once tripped
# (requeue, no attempt / attempt-charged streak) is test-thrash-teardown.sh (G2). This
# suite is left with what those two do not cover: cockpit-collect metrics's own thrash counter
# and the SPIRA_THRASH_MINUTES conf.d default.
#
# tier: T1
# covers: dwarf/src/trace.rs cockpit-collect/src/* cockpit-collect/src/metrics.rs UC-dwarf-execution-09
# scar: unrecorded
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---- Part 5: cockpit-collect metrics counts requeue-thrash ledger lines ------------------
echo
echo "cockpit-collect metrics: status=requeue-thrash is counted as SP_DWARF_THRASH"

COLLECT="$(command -v cockpit-collect)"
if [ -z "$COLLECT" ]; then
    bad "cockpit-collect on PATH" "not found"
else
    LEDGER="$TMP/dwarf-ledger.log"
    SENTINEL="$TMP/sentinel.log"
    printf '' > "$SENTINEL"
    printf '' > "$LEDGER"

    call_ledger() { "$COLLECT" metrics "$SENTINEL" "$LEDGER" 24 2>/dev/null; }

    # key_val <key> <output>: extract value for a key from the output.
    key_val() { grep "^$1=" <<< "$2" | cut -d= -f2; }

    # POSITIVE CONTROL: an empty ledger must return SP_DWARF_THRASH=0, not missing.
    out0="$(call_ledger)"
    zero_thrash="$(key_val SP_DWARF_THRASH "$out0")"
    is "empty ledger → SP_DWARF_THRASH=0 (positive control)" "0" "$zero_thrash"

    # Now add timestamped ledger lines (cockpit-collect metrics expects ISO timestamps).
    NOW="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    printf '%s awake rune sp-aaa worked\n' "$NOW" >> "$LEDGER"
    printf '%s done rune sp-bbb rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done rune sp-ccc rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done rune sp-ddd rc=0 status=requeue-thrash\n' "$NOW" >> "$LEDGER"
    printf '%s done rune sp-eee rc=0 status=requeue-slain\n' "$NOW" >> "$LEDGER"
    out3="$(call_ledger)"
    thrash_count="$(key_val SP_DWARF_THRASH "$out3")"
    is "three requeue-thrash lines → SP_DWARF_THRASH=3" "3" "$thrash_count"

    # Verify other counters are not contaminated.
    worked_count="$(key_val SP_DWARF_WORKED "$out3")"
    is "thrash lines do not inflate SP_DWARF_WORKED" "1" "$worked_count"
fi

# ---- Part 6: SPIRA_THRASH_MINUTES in the conf.d registry --------------------------------
echo
echo "conf.d: SPIRA_THRASH_MINUTES has a default value"

# conf.sh's keys and defaults are generated from spira/conf.d/, one file per key (sp-g3uwp).
KEYF="$HERE/conf.d/SPIRA_THRASH_MINUTES"
if [ ! -f "$KEYF" ]; then
    bad "conf.d registers SPIRA_THRASH_MINUTES" "no $KEYF"
else
    ok "conf.d registers SPIRA_THRASH_MINUTES"
    default_val="$(sed -n "/^DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'/,/^SPIRA_CONF_DEFAULT_EOF/p" "$KEYF" | grep -o ':=[0-9]\+' | tr -d ':=' | head -1)"
    case "${default_val:-}" in
        [0-9]*) ok "SPIRA_THRASH_MINUTES default is a number ($default_val)" ;;
        *)      bad "SPIRA_THRASH_MINUTES default is a number" "got [${default_val:-}]" ;;
    esac
fi

tl_summary
