#!/usr/bin/env bash
# timeout: 150
#
# test-groomer-sweep.sh — groomer.sh sweep: mechanical livelock remedies before the model pass.
#
# FOUR CATEGORIES, ONE PAIR (law-absence-needs-a-positive-control)
# ----------------------------------------------------------------
# The fixture seeds all four livelock categories plus a described unmapped-repo bead.
# The pair that proves the predicate:
#   sp-sw-lit  — unmapped-repo, no description, no notes → CLOSED (acted)
#   sp-sw-desc — unmapped-repo, has description          → REPORT only (not acted)
#
# DRY-RUN FIRST, REAL RUN SECOND. The dry-run verifies the right output without
# changing state; the real run applies the remedies and the after-run check verifies
# the detector now returns only the two beads sweep cannot fix.
#
# tier: T1
# covers: spira/groomer.sh spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-groomer-sweep
TMP="$(mktemp -d)"
testdb_up sweep || { echo "test-groomer-sweep: could not build fixture database"; exit 1; }
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'exit 143' INT TERM

. "$HERE/testlib.sh"

GROOMSH="$HERE/groomer.sh"
MAP="$TMP/repo-map"
RUN="$TMP/run"; mkdir -p "$RUN"

# PINNED TO NON-DEFAULT NAMES so the suite cannot pass on an accidentally matching literal.
printf 'pushrepo | /opt/pushrepo | push | origin/main | | \n' > "$MAP"
printf 'prerepo  | /opt/prerepo  | pr   | origin/main | | \n' >> "$MAP"

SCOPE=spira

run_sweep() {
    env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
        SPIRA_PATH="${SPIRA_PATH:-}" \
        SPIRA_BD="${SPIRA_BD:-bd}" \
        SPIRA_DB="$SPIRA_DB" \
        SPIRA_RUN="$RUN" \
        SPIRA_REPO_MAP="$MAP" \
        SPIRA_GOAL=sp-goal \
        SPIRA_ASK_LABEL=needs-ryan \
        SPIRA_CI_LABEL=awaiting-ci \
        SPIRA_SPIKE_LABEL=spike \
        SPIRA_SCOPE_LABEL="$SCOPE" \
        bash "$GROOMSH" sweep "$@" 2>&1
}

status_of() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | python3 -c '
import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("status",""))' 2>/dev/null
}

labels_of() {
    bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '
}

echo "test-groomer-sweep.sh"

# ==========================================================================================
echo
echo "NEGATIVE CONTROL — empty database: sweep reports no livelocked beads"
# ==========================================================================================
testdb_reset
: > "$RUN/groom.log"
out="$(run_sweep --dry-run)"
is "empty db: exits 0" 0 "$?"
want "empty db: no livelocked output" "no livelocked" "$out"
nowant "empty db: no CLOSED in groom.log" "CLOSED" "$(cat "$RUN/groom.log" 2>/dev/null)"

# ==========================================================================================
echo
echo "FIXTURE — seed all four livelock categories plus a described unmapped-repo bead"
# ==========================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-sw-unc","title":"unclaimable fayth:ops on plan","status":"open","issue_type":"task","labels":["fayth:ops","plan","repo:pushrepo","$SCOPE"]}
{"id":"sp-sw-nr","title":"needs-ryan no overseer","status":"open","issue_type":"task","labels":["needs-ryan","$SCOPE","plan","repo:pushrepo"]}
{"id":"sp-sw-ci","title":"ci-stuck push repo","status":"open","issue_type":"task","labels":["awaiting-ci","$SCOPE","plan","repo:pushrepo"]}
{"id":"sp-sw-lit","title":"litter bead","status":"open","issue_type":"task","labels":["$SCOPE","plan","repo:bogusrepo"]}
{"id":"sp-sw-desc","title":"described unmapped bead","status":"open","issue_type":"task","labels":["$SCOPE","plan","repo:bogusrepo"],"description":"This bead has a real description and should not be closed by sweep."}
JSONL

# ==========================================================================================
echo
echo "DRY-RUN — three actions named, one report-only, described bead left alone"
# ==========================================================================================
# Pair: sp-sw-lit (acted) / sp-sw-desc (not acted) both unmapped-repo.
: > "$RUN/groom.log"
out="$(run_sweep --dry-run)"
is "dry-run: exits 0" 0 "$?"

want "dry-run: OVERSEER action for needs-ryan bead"  "OVERSEER sp-sw-nr"   "$out"
want "dry-run: UNSTUCK action for ci-stuck bead"     "UNSTUCK sp-sw-ci"    "$out"
want "dry-run: CLOSED action for litter bead"        "CLOSED sp-sw-lit"    "$out"
want "dry-run: REPORT for unclaimable bead"          "REPORT sp-sw-unc"    "$out"
want "dry-run: unclaimable named"                    "unclaimable"         "$out"

# Described bead: reported (not acted)
want  "dry-run: described bead in output"         "sp-sw-desc"         "$out"
nowant "dry-run: described bead not closed"       "CLOSED sp-sw-desc"  "$out"

# Dry-run must not change any state
is "dry-run: litter bead still open"   "open" "$(status_of sp-sw-lit)"
is "dry-run: described bead still open" "open" "$(status_of sp-sw-desc)"
nowant "dry-run: overseer NOT added to needs-ryan bead" "overseer" "$(labels_of sp-sw-nr)"
want  "dry-run: awaiting-ci still on ci-stuck bead"    "awaiting-ci" "$(labels_of sp-sw-ci)"

# ==========================================================================================
echo
echo "REAL RUN — sweep applies the three mechanical remedies"
# ==========================================================================================
: > "$RUN/groom.log"
out="$(run_sweep)"
is "real run: exits 0" 0 "$?"

# overseer label added
want "real: overseer added to needs-ryan bead" "overseer" "$(labels_of sp-sw-nr)"

# awaiting-ci label removed
nowant "real: awaiting-ci removed from ci-stuck bead" "awaiting-ci" "$(labels_of sp-sw-ci)"

# litter bead closed
is "real: litter bead closed" "closed" "$(status_of sp-sw-lit)"

# litter close reason names the predicate facts
reason_lit="$(bd -C "$SPIRA_DB" show sp-sw-lit --json 2>/dev/null | python3 -c '
import json,sys; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
print(d[0].get("close_reason","") or "")' 2>/dev/null)"
want "real: litter close reason names predicate"     "litter"         "$reason_lit"
want "real: litter close reason names no description" "no description" "$reason_lit"

# described bead still open
is "real: described bead still open" "open" "$(status_of sp-sw-desc)"

# ==========================================================================================
echo
echo "groom.log written during real run"
# ==========================================================================================
groom_log="$(cat "$RUN/groom.log" 2>/dev/null)"
want "groom.log has OVERSEER action" "groom: sweep: OVERSEER" "$groom_log"
want "groom.log has UNSTUCK action"  "groom: sweep: UNSTUCK"  "$groom_log"
want "groom.log has CLOSED action"   "groom: sweep: CLOSED"   "$groom_log"

# ==========================================================================================
echo
echo "AFTER REAL RUN — detector returns only unclaimable and described unmapped-repo"
# ==========================================================================================
after_ll="$(env -i PATH="$PATH" HOME="$HOME" LC_ALL=C.UTF-8 \
    SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" SPIRA_REPO="$TMP" \
    SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-goal \
    SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=awaiting-ci \
    SPIRA_SPIKE_LABEL=spike \
    SPIRA_SCOPE_LABEL="$SCOPE" \
    bash "$HERE/cockpit.sh" livelock 2>/dev/null)"

want  "after sweep: unclaimable bead still reported"       "sp-sw-unc"  "$after_ll"
want  "after sweep: described bead still reported"         "sp-sw-desc" "$after_ll"
nowant "after sweep: needs-ryan bead no longer reported"   "sp-sw-nr"   "$after_ll"
nowant "after sweep: ci-stuck bead no longer reported"     "sp-sw-ci"   "$after_ll"
nowant "after sweep: litter bead no longer reported"       "sp-sw-lit"  "$after_ll"

echo
tl_summary
