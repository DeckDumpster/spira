#!/usr/bin/env bash
#
# test-cockpit-sop.sh — SP_SOP_NEVER_FIRED and SP_SOP_RECURRED render correctly.
#
#   ./test-cockpit-sop.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# cockpit.sh now emits two SOP metrics into the snapshot:
#
#   SP_SOP_NEVER_FIRED   SOPs on the shelf with no ledger entry — never exercised.
#   SP_SOP_RECURRED      SOPs applied (check=pass) where held=no in the window — the fix
#                        did not hold.
#
# Both must render `?` when their inputs are unreadable, never 0. The failure mode this
# exists to prevent is a broken probe reading as "no dead-weight SOPs, no recurrences" —
# the all-clear that stops anybody looking (law-absence-needs-a-positive-control).
#
# THE POSITIVE CONTROL IS A REAL NEGATIVE. Every assertion that a probe returns `?` is
# preceded, in the same fixture, by the same probe returning a real number — so a probe
# that always returns `?` fails the positive half, and a probe that never returns `?`
# fails the negative half. Both halves are required.
#
# A REAL `bd` ON A THROWAWAY DATABASE. The probe reads `bd memories --json` from the
# shelf, and a stub `bd` would not reproduce what the real binary returns under error
# conditions — which is the exact case this suite must assert against. The shelf case
# (broken database) depends on `bd` returning the empty string on failure, which it does,
# and no stub can promise to do the same when the code evolves.
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

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-cockpit-sop
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up sop || { echo "test-cockpit-sop: could not build fixture database"; exit 1; }

RUN="$TMP/run"; mkdir -p "$RUN"
LEDGER="$TMP/ledger/applied.jsonl"; mkdir -p "$(dirname "$LEDGER")"

# sop.sh in a minimal environment, pointing at the fixture database.
# SPIRA_WIKI is absent: `synth` exits 0 without writing anything, so `write` succeeds.
# SPIRA_SOP_LEDGER points at our test ledger, not the one under SPIRA_RUN.
sop_run() {
    env -i HOME="$HOME" PATH="$PATH" SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_CONF="$TMP/no.conf" SPIRA_DB="$SPIRA_DB" SPIRA_RUN="$RUN" \
        SPIRA_SOP_LEDGER="$LEDGER" BEADS_NO_AUTO_IMPORT=1 \
        bash "$HERE/sop.sh" "$@" 2>/dev/null
}

# cockpit.sh sops — just the SOP section, not the full probe. The seam exists for this
# purpose: it is the same function probe() calls, so what is tested is what runs.
run_sops() {    # run_sops [env KEY=val ...] — extra env entries are prepended before bash
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_PATH="${SPIRA_PATH:-}" SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" SPIRA_RUN="$RUN" \
        SPIRA_DB="$SPIRA_DB" SPIRA_REPO_MAP="$TMP/no-map" \
        SPIRA_GOAL=sp-test SPIRA_FAYTHS=t \
        SPIRA_SOP_LEDGER="$LEDGER" \
        "$@" \
        bash "$HERE/cockpit.sh" sops 2>/dev/null
}

# Write two SOPs to the fixture database.
sop_run write disk-full - <<'SOP' >/dev/null
MATCH: (disk.full|No space left)
SYMPTOM: a unit fails because the volume it writes to is full
CHECK: df -h /var | tail -1
FIX: clear the oldest artifacts and restart the unit
SOP
sop_run write clock-skew - <<'SOP' >/dev/null
MATCH: clock.skew
SYMPTOM: a unit fails because the box clock moved
CHECK: timedatectl show -p NTPSynchronized
FIX: restart the time-sync unit
SOP

# Helpers to write a ledger entry directly, bypassing sop.sh applied — the cockpit probe
# reads the ledger file, not the bead notes, so the bead does not need to exist.
ledger_entry() {    # ledger_entry <sop-key> <check> <held> <ts-iso8601>
    local k="$1" chk="$2" hld="$3" ts="$4"
    local epoch; epoch="$(date -d "$ts" +%s 2>/dev/null)" || epoch=0
    printf '{"ts":"%s","epoch":%s,"sop":"%s","bead":"sp-fixture","check":"%s","held":"%s","actor":"test","shelf":"ok","note":"ok","why":""}\n' \
        "$ts" "$epoch" "$k" "$chk" "$hld" >> "$LEDGER"
}

# ======================================================================================
echo
echo "no ledger — every SOP is never-fired:"

out="$(run_sops)"
is "SP_SOP_NEVER_FIRED=2 when no ledger exists" "SP_SOP_NEVER_FIRED=2" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
is "SP_SOP_RECURRED=0 when no ledger exists"    "SP_SOP_RECURRED=0"    "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

# ======================================================================================
echo
echo "disk-full applied and held — disk-full no longer never-fired, clock-skew still is:"

# Record disk-full as applied and held (the SOP worked).
now_ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
ledger_entry "sop-disk-full" pass yes "$now_ts"

out="$(run_sops)"
is "SP_SOP_NEVER_FIRED=1 — clock-skew still unfired" "SP_SOP_NEVER_FIRED=1" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
is "SP_SOP_RECURRED=0 — held=yes means it worked"    "SP_SOP_RECURRED=0"    "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

# ======================================================================================
echo
echo "disk-full applied but did not hold — recurrence counted:"

ledger_entry "sop-disk-full" pass no "$now_ts"

out="$(run_sops)"
is "SP_SOP_NEVER_FIRED=1 — clock-skew still unfired" "SP_SOP_NEVER_FIRED=1" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
is "SP_SOP_RECURRED=1 — disk-full did not hold"      "SP_SOP_RECURRED=1"    "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

# ======================================================================================
echo
echo "clock-skew held=no but outside the window — not a recurrence:"

# Write an old entry: epoch 0 is 1970, well outside any 24h window.
ledger_entry "sop-clock-skew" pass no "1970-01-01T00:00:00Z"

out="$(run_sops)"
# clock-skew now has a ledger entry, so never-fired drops to 0.
is "SP_SOP_NEVER_FIRED=0 — both SOPs have entries" "SP_SOP_NEVER_FIRED=0" "$(grep 'SP_SOP_NEVER_FIRED=' <<< "$out")"
# The clock-skew entry is outside the window, so only disk-full counts as recurred.
is "SP_SOP_RECURRED=1 — old entry is outside the window" "SP_SOP_RECURRED=1" "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

# ======================================================================================
echo
echo "POSITIVE CONTROL: broken shelf renders ? not 0:"

# A nonexistent database makes bdjson memories return the empty string. The probe must
# render ? rather than 0 — a database outage must not read as "all SOPs have fired".
out="$(run_sops env SPIRA_DB="$TMP/no-such-db")"
want   "broken shelf: SP_SOP_NEVER_FIRED=?" "SP_SOP_NEVER_FIRED=?" "$out"
want   "broken shelf: SP_SOP_RECURRED=?"    "SP_SOP_RECURRED=?"    "$out"
nowant "broken shelf never reports 0 for never-fired" "SP_SOP_NEVER_FIRED=0" "$out"
nowant "broken shelf never reports 0 for recurred"    "SP_SOP_RECURRED=0"    "$out"

# THE POSITIVE CONTROL FOR THE POSITIVE CONTROL: the same run against the GOOD db still
# gets real counts, so the ?s above are a response to the bad database, not a broken probe
# that always outputs ?.
out_good="$(run_sops)"
nowant "good db: SP_SOP_NEVER_FIRED is not ?" "SP_SOP_NEVER_FIRED=?" "$out_good"
nowant "good db: SP_SOP_RECURRED is not ?"    "SP_SOP_RECURRED=?"    "$out_good"

# ======================================================================================
echo
echo "POSITIVE CONTROL: unreadable ledger (directory in place of file) renders ? not 0:"

# A directory at the ledger path causes open() to raise IsADirectoryError, which the
# probe catches and converts to ?.
mkdir -p "$TMP/dir-not-file.jsonl"
out="$(run_sops env SPIRA_SOP_LEDGER="$TMP/dir-not-file.jsonl")"
want   "unreadable ledger: SP_SOP_NEVER_FIRED=?" "SP_SOP_NEVER_FIRED=?" "$out"
want   "unreadable ledger: SP_SOP_RECURRED=?"    "SP_SOP_RECURRED=?"    "$out"

# THE POSITIVE CONTROL FOR THAT: the SAME db with the GOOD ledger still returns counts.
out_good2="$(run_sops)"
nowant "good ledger: SP_SOP_NEVER_FIRED is not ?" "SP_SOP_NEVER_FIRED=?" "$out_good2"
nowant "good ledger: SP_SOP_RECURRED is not ?"    "SP_SOP_RECURRED=?"    "$out_good2"

# ======================================================================================
echo
echo "check=fail entry does not count as recurrence (the FIX was never run):"

# A check=fail entry records that the SOP did not apply to this incident — not that the
# fix failed. It should not count as a recurrence (held is implicitly not-applicable).
ledger_entry "sop-disk-full" fail unknown "$now_ts"

out="$(run_sops)"
is "SP_SOP_RECURRED stays 1 after a check=fail entry" "SP_SOP_RECURRED=1" "$(grep 'SP_SOP_RECURRED=' <<< "$out")"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
