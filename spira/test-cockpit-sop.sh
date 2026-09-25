#!/usr/bin/env bash
#
# test-cockpit-sop.sh — SP_SOP_NEVER_FIRED, SP_SOP_RECURRED and SP_SWEEP_AGE render
# correctly.
#
#   ./test-cockpit-sop.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# cockpit.sh emits three SOP metrics into the snapshot:
#
#   SP_SOP_NEVER_FIRED   SOPs on the shelf with no ledger entry — never exercised.
#   SP_SOP_RECURRED      SOPs applied (check=pass) where held=no in the window — the fix
#                        did not hold.
#   SP_SWEEP_AGE         seconds since the newest ledger entry — a stopped sweep and a
#                        quiet one must be distinguishable (law-arm-before-you-retire).
#
# All three must render `?` when their inputs are unreadable, never 0. The failure mode this
# exists to prevent is a broken probe reading as "no dead-weight SOPs, no recurrences, sweep
# just ran" — the all-clear that stops anybody looking (law-absence-needs-a-positive-control).
#
# THE POSITIVE CONTROL IS A REAL NEGATIVE. Every assertion that a probe returns `?` is
# preceded, in the same fixture, by the same probe returning a real number — so a probe
# that always returns `?` fails the positive half, and a probe that never returns `?`
# fails the negative half. Both halves are required.
#
# SPIRA_BDJSON_FIXTURE, NOT A REAL STORE (docs/test-plan/cockpit-observability.md coverage
# row 14). sop_keys' only bd read is bare `bd memories`, which returns the shelf as one
# JSON object; a canned fixture reproduces that shape exactly, and the "broken shelf" case
# below is reproduced by pointing the fixture at a path that does not exist — bdsim.py exits
# with empty stdout, the same shape bdjson sees from a bd that cannot reach its store.
#
# SPIRA_NOW PINS THE CLOCK for SP_SWEEP_AGE, so "fresh" and "stale" are exact equalities
# instead of a wall-clock race against how long the suite takes to run.
#
# ONE SUITE, NOT TWO (docs/test-plan/cockpit-observability.md duplicate cluster 5).
# test-cockpit-sweep.sh used to duplicate this harness — same run_sops helper, same ledger
# format, same broken-shelf and dir-as-ledger fixtures — to test SP_SWEEP_AGE alone. Merged
# here.
#
# defect: sp-wwav
# covers: spira/cockpit.sh spira/sop.sh
# covers: spira/cockpit-metrics.py
# scar: SP_SOP_NEVER_FIRED and SP_SOP_RECURRED were absent from the snapshot; a broken probe rendered as all-clear for dead-weight runbooks and recurring incidents.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-cockpit-sop.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
RUN="$TMP/run"; mkdir -p "$RUN"
LEDGER="$TMP/ledger/applied.jsonl"; mkdir -p "$(dirname "$LEDGER")"
NOW="$(date +%s)"

# Two SOPs on the shelf — the only shape sop_keys reads from `bd memories`: an object whose
# "sop-*" keys name the SOPs, regardless of value.
cat > "$TMP/shelf.json" <<'JSON'
{"memories":{"sop-disk-full":"MATCH: disk.full\nFIX: clear the oldest artifacts","sop-clock-skew":"MATCH: clock.skew\nFIX: restart the time-sync unit"}}
JSON

# cockpit.sh sops — just the SOP section, not the full probe. The seam exists for this
# purpose: it is the same function probe() calls, so what is tested is what runs.
run_sops() {    # run_sops [env KEY=val ...] — extra env entries are prepended before bash
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" SPIRA_RUN="$RUN" \
        SPIRA_DB="$TMP/nodb" SPIRA_BDJSON_FIXTURE="$TMP/shelf.json" \
        SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        SPIRA_SOP_LEDGER="$LEDGER" SPIRA_NOW="$NOW" \
        "$@" \
        bash "$HERE/cockpit.sh" sops 2>/dev/null
}

# Write a ledger entry directly — the cockpit probe reads the file, not a bead.
ledger_entry() {    # ledger_entry <sop-key> <check> <held> <epoch>
    local k="$1" chk="$2" hld="$3" ep="$4" ts
    ts="$(date -u -d "@$ep" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null)" || ts="1970-01-01T00:00:00Z"
    printf '{"ts":"%s","epoch":%s,"sop":"%s","bead":"sp-fixture","check":"%s","held":"%s","actor":"test","shelf":"ok","note":"ok","why":""}\n' \
        "$ts" "$ep" "$k" "$chk" "$hld" >> "$LEDGER"
}

# ======================================================================================
echo
echo "no ledger — every SOP is never-fired, and SP_SWEEP_AGE=? (sweep never ran):"

out="$(run_sops)"
is "SP_SOP_NEVER_FIRED=2 when no ledger exists" "SP_SOP_NEVER_FIRED=2" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
is "SP_SOP_RECURRED=0 when no ledger exists"    "SP_SOP_RECURRED=0"    "$(grep 'SP_SOP_RECURRED=' <<< "$out")"
want   "missing ledger: SP_SWEEP_AGE=?"    "SP_SWEEP_AGE=?" "$out"
nowant "missing ledger: not a number"      "SP_SWEEP_AGE=0" "$out"

# ======================================================================================
echo
echo "empty ledger (file exists, no entries) — same as no ledger for SP_SWEEP_AGE:"

touch "$LEDGER"
out="$(run_sops)"
want   "empty ledger: SP_SWEEP_AGE=?"   "SP_SWEEP_AGE=?" "$out"
nowant "empty ledger: not a number"     "SP_SWEEP_AGE=0" "$out"

# ======================================================================================
echo
echo "disk-full applied and held — disk-full no longer never-fired, clock-skew still is, fresh sweep:"

ledger_entry "sop-disk-full" pass yes "$NOW"

out="$(run_sops)"
is "SP_SOP_NEVER_FIRED=1 — clock-skew still unfired" "SP_SOP_NEVER_FIRED=1" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
is "SP_SOP_RECURRED=0 — held=yes means it worked"    "SP_SOP_RECURRED=0"    "$(grep 'SP_SOP_RECURRED=' <<< "$out")"
is "SP_SWEEP_AGE=0 — the ledger entry is at SPIRA_NOW" "SP_SWEEP_AGE=0" "$(grep 'SP_SWEEP_AGE=' <<< "$out")"

# ======================================================================================
echo
echo "disk-full applied but did not hold — recurrence counted:"

ledger_entry "sop-disk-full" pass no "$NOW"

out="$(run_sops)"
is "SP_SOP_NEVER_FIRED=1 — clock-skew still unfired" "SP_SOP_NEVER_FIRED=1" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
is "SP_SOP_RECURRED=1 — disk-full did not hold"      "SP_SOP_RECURRED=1"    "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

# ======================================================================================
echo
echo "clock-skew held=no but outside the window — not a recurrence, and the sweep reads stale:"

# An entry 2 hours before SPIRA_NOW: outside the 24h window is still "fired" (never-fired
# drops), but well outside a re-narrowed window would not count as a recurrence either way
# here it is inside 24h, so assert the boundary the suite actually exercises: an entry from
# 1970 is outside any window and also becomes the SWEEP_AGE floor test below.
ledger_entry "sop-clock-skew" pass no 0

out="$(run_sops)"
# clock-skew now has a ledger entry, so never-fired drops to 0.
is "SP_SOP_NEVER_FIRED=0 — both SOPs have entries" "SP_SOP_NEVER_FIRED=0" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
# The 1970 entry is outside the window, so only disk-full counts as recurred.
is "SP_SOP_RECURRED=1 — old entry is outside the window" "SP_SOP_RECURRED=1" "$(grep 'SP_SOP_RECURRED=' <<< "$out")"
# The NEWEST entry (disk-full, at SPIRA_NOW) still wins: the sweep still reads fresh.
is "SP_SWEEP_AGE=0 — newest entry still wins" "SP_SWEEP_AGE=0" "$(grep 'SP_SWEEP_AGE=' <<< "$out")"

# ======================================================================================
echo
echo "only a stale entry on the shelf — SP_SWEEP_AGE is large (stale sweep):"

> "$LEDGER"
old_ep=$(( NOW - 7200 ))
ledger_entry "sop-disk-full" pass yes "$old_ep"

out="$(run_sops)"
nowant "backdated entry: SP_SWEEP_AGE is not ?" "SP_SWEEP_AGE=?" "$out"
is "backdated entry: SP_SWEEP_AGE=7200 exactly (SPIRA_NOW pinned)" "SP_SWEEP_AGE=7200" \
   "$(grep 'SP_SWEEP_AGE=' <<< "$out")"

# POSITIVE CONTROL FOR STALE: re-add a fresh entry — the age should shrink back to 0.
ledger_entry "sop-disk-full" pass yes "$NOW"
out_fresh="$(run_sops)"
is "newest-wins: fresh entry after old one gives SP_SWEEP_AGE=0" "SP_SWEEP_AGE=0" \
   "$(grep 'SP_SWEEP_AGE=' <<< "$out_fresh")"

# ======================================================================================
echo
echo "POSITIVE CONTROL: broken shelf renders ? for all three, not 0:"

# A nonexistent fixture path makes bdjson memories return the empty string — the same shape
# a real bd sees when it cannot reach its store. The probe must render ? rather than
# "all SOPs have fired, sweep just ran".
out="$(run_sops env SPIRA_BDJSON_FIXTURE="$TMP/no-such-shelf.json")"
want   "broken shelf: SP_SOP_NEVER_FIRED=?" "SP_SOP_NEVER_FIRED=?" "$out"
want   "broken shelf: SP_SOP_RECURRED=?"    "SP_SOP_RECURRED=?"    "$out"
want   "broken shelf: SP_SWEEP_AGE=?"       "SP_SWEEP_AGE=?"       "$out"
nowant "broken shelf never reports 0 for never-fired" "SP_SOP_NEVER_FIRED=0" "$out"
nowant "broken shelf never reports 0 for recurred"    "SP_SOP_RECURRED=0"    "$out"
nowant "broken shelf never reports 0 for sweep age"   "SP_SWEEP_AGE=0"       "$out"

# THE POSITIVE CONTROL FOR THE POSITIVE CONTROL: the same run against the GOOD shelf still
# gets real counts, so the ?s above are a response to the bad shelf, not a broken probe
# that always outputs ?.
out_good="$(run_sops)"
nowant "good shelf: SP_SOP_NEVER_FIRED is not ?" "SP_SOP_NEVER_FIRED=?" "$out_good"
nowant "good shelf: SP_SOP_RECURRED is not ?"    "SP_SOP_RECURRED=?"    "$out_good"
nowant "good shelf: SP_SWEEP_AGE is not ?"       "SP_SWEEP_AGE=?"       "$out_good"

# ======================================================================================
echo
echo "POSITIVE CONTROL: unreadable ledger (directory in place of file) renders ? not 0:"

# A directory at the ledger path causes open() to raise IsADirectoryError, which the
# probe catches and converts to ?.
mkdir -p "$TMP/dir-not-file.jsonl"
out="$(run_sops env SPIRA_SOP_LEDGER="$TMP/dir-not-file.jsonl")"
want "unreadable ledger: SP_SOP_NEVER_FIRED=?" "SP_SOP_NEVER_FIRED=?" "$out"
want "unreadable ledger: SP_SOP_RECURRED=?"    "SP_SOP_RECURRED=?"    "$out"
want "unreadable ledger: SP_SWEEP_AGE=?"       "SP_SWEEP_AGE=?"       "$out"

# THE POSITIVE CONTROL FOR THAT: the SAME shelf with the GOOD ledger still returns counts.
out_good2="$(run_sops)"
nowant "good ledger: SP_SOP_NEVER_FIRED is not ?" "SP_SOP_NEVER_FIRED=?" "$out_good2"
nowant "good ledger: SP_SOP_RECURRED is not ?"    "SP_SOP_RECURRED=?"    "$out_good2"
nowant "good ledger: SP_SWEEP_AGE is not ?"       "SP_SWEEP_AGE=?"       "$out_good2"

# ======================================================================================
echo
echo "check=fail entry does not count as recurrence (the FIX was never run):"

# A check=fail entry records that the SOP did not apply to this incident — not that the
# fix failed. It should not count as a recurrence (held is implicitly not-applicable).
ledger_entry "sop-disk-full" fail unknown "$NOW"

out="$(run_sops)"
is "SP_SOP_RECURRED stays 0 after a check=fail entry" "SP_SOP_RECURRED=0" "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
