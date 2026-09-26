#!/usr/bin/env bash
#
# test-unclaimable.sh — detect_unclaimable_ready surfaces ready beads no persona can claim,
#   and its cycle guard stops the detector from reporting its own reports.
#
# THE DEFECT (sp-f8vry). detect_unclaimable_ready reads all ready beads without a partition
# filter, tests each against the full chamber using the same claimers() arithmetic as
# bead.sh, and emits UNCLAIMABLE for any with an empty intersection. Seven P1 beads carried
# fayth:ops on spira,plan labels for fifteen hours while the sentinel reported every
# partition empty, truthfully, on every pass.
#
# THE CYCLE GUARD. A report about an unclaimable bead is filed into the incident partition;
# when that partition is unservable the report is itself unclaimable, and reporting it would
# file another report ad infinitum. Measured 2026-09-12: one watchtower incident seeded a
# 128-deep chain, 58 beads to 310 in minutes. external_ref "unclaimable:<subject>" is the
# base case.
#
# MERGED FROM (D6, sp-9ce60.2.3): test-unclaimable-bead.sh (12 cases over real bd, 47s) and
# test-unclaimable-cycle.sh (hermetic but string-split lib.sh to reach the python body, 72s
# measurement anomaly). The classifier now lives in its own file, spira/unclaimable.py, so
# T1 below drives it directly over canned JSON; only one T2 row exercises the real bd-ready
# shape, and the incident-filing rows drive file_unclaimable_incidents against a mock
# incident.sh (no bd needed at all — the input is a canned UNCLAIMABLE line).
#
# EVERY T1 ROW HAS A POSITIVE CONTROL (law-absence-needs-a-positive-control): a case that
# proves the classifier CAN emit UNCLAIMABLE sits beside every case proving it does not,
# so a detector that reports everything, or nothing, fails half its own table.
#
# tier: T1
# defect: sp-9zyu1 sp-f8vry
# covers: spira/lib.sh spira/unclaimable.py spira/sentinel.sh UC-dispatch-16
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-unclaimable.sh"

# ==============================================================================================
# T1 — the classifier (spira/unclaimable.py) driven directly over fixture JSON, no bd at all.
# ==============================================================================================

# Non-default labels throughout (law-gates-run-in-a-clean-environment): a hardcoded "spira",
# "needs-operator" or "awaiting-ci" in the classifier would pass against the shipped defaults
# and fail here.
SCOPE=myscope
ASK=needs-decision
CI=ci-parked
GROOM_ASK=parked-for-ryan

# Small chamber: builder (plan), ops (incident), groom (groom), maechen (maechen-sweep) and
# spike (spike). Mirrors chamber/*.fayth's FAYTH_LABELS/FAYTH_EXCLUDE_LABELS shape without
# reading the real chamber files, so the fixture cannot drift silently with a chamber edit.
EXC="spira-poison,${ASK},${CI}"
FULL_PARTS="builder|${SCOPE},plan|${EXC}
ops|${SCOPE},incident|${EXC}
groom|${SCOPE},groom|${EXC}
maechen|${SCOPE},maechen-sweep|${EXC}
spike|${SCOPE},spike|${EXC}
"
# Narrowed roster (as SPIRA_FAYTHS="builder spike" would produce): only two personas active.
NARROW_PARTS="builder|${SCOPE},plan|${EXC}
spike|${SCOPE},spike|${EXC}
"

# classify <beads-json> [parts] [all_parts] -> unclaimable.py's stdout
classify() {
    local beads_json="$1" parts="${2:-$FULL_PARTS}" all_parts="${3:-$FULL_PARTS}"
    PARTS="$parts" ALL_PARTS="$all_parts" \
    SPIRA_SCOPE_LABEL="$SCOPE" SPIRA_CI_LABEL="$CI" SPIRA_ASK_LABEL="$ASK" \
    SPIRA_GROOM_ASK_LABEL="$GROOM_ASK" \
        python3 "$HERE/unclaimable.py" <<< "$beads_json"
}

bead() { # bead <id> <extra-labels-csv-or-""> -> one bead JSON object, always scoped+repo'd
    local id="$1" extra="$2" labels="\"repo:spira\",\"$SCOPE\""
    [ -n "$extra" ] && labels="$labels,$(printf '%s' "$extra" | sed 's/,/","/g;s/^/"/;s/$/"/')"
    printf '{"id":"%s","title":"t %s","status":"open","issue_type":"task","labels":[%s]}' "$id" "$id" "$labels"
}

echo
echo "case 1 — positive control: a claimable bead (builder: scope,plan) is NOT flagged"
out="$(classify "[$(bead sp-unc1a plan)]")"
nowant "claimable builder bead is not flagged" "sp-unc1a" "$out"

echo
echo "case 2 — fayth:ops on scope,plan labels is UNCLAIMABLE (the fifteen-hour strand)"
out="$(classify "[$(bead sp-unc2a "fayth:ops,plan")]")"
want "fayth:ops on scope,plan: UNCLAIMABLE line emitted" "UNCLAIMABLE sp-unc2a" "$out"
want "fayth:ops on scope,plan: names the preference"      "fayth:ops"            "$out"
want "fayth:ops on scope,plan: names the rejection"       "ops"                  "$out"

echo
echo "case 3 — scope with no partition label is UNCLAIMABLE (the sp-bvo7 route)"
out="$(classify "[$(bead sp-unc3a "")]")"
want "scope no partition: UNCLAIMABLE line emitted" "UNCLAIMABLE sp-unc3a" "$out"
want "scope no partition: names a partition to add" "plan"                 "$out"

echo
echo "case 4 — idle queue: no beads at all returns no UNCLAIMABLE lines (not a false alarm)"
out="$(classify "[]")"
is "empty queue: no UNCLAIMABLE output" "" "$out"

echo
echo "case 5 — mixed queue: unclaimable bead flagged, claimable neighbour is not"
out="$(classify "[$(bead sp-unc5a plan), $(bead sp-unc5b "")]")"
nowant "mixed queue: claimable bead not flagged"  "sp-unc5a"             "$out"
want   "mixed queue: unclaimable bead is flagged" "UNCLAIMABLE sp-unc5b" "$out"

echo
echo "case 6 — no-scope-label bead is UNCLAIMABLE; in-scope+no-partition bead is also flagged"
# detect_unclaimable_ready's own bd query has no scope filter (it reads the raw ready set —
# see the doc comment on the function), so a bead from a foreign repo lacking the scope
# label entirely can reach the classifier here, with the normal non-empty scope label.
out="$(classify '[{"id":"pd-unc6a","title":"no scope label","status":"open","labels":["plan","repo:pokedumpster"]}]')"
want "no-scope-label bead reported UNCLAIMABLE (missing scope label)" "UNCLAIMABLE pd-unc6a" "$out"
out2="$(classify "[$(bead sp-unc6c "")]")"
want "in-scope no-partition bead IS reported (positive control)" "UNCLAIMABLE sp-unc6c" "$out2"

echo
echo "case 7 — spira-poison and ask-label beads are excluded (have their own check)"
out="$(classify "[$(bead sp-unc7a spira-poison), $(bead sp-unc7b "$ASK")]")"
nowant "poisoned bead not flagged by unclaimable check"   "sp-unc7a" "$out"
nowant "ask-label bead not flagged by unclaimable check"  "sp-unc7b" "$out"

echo
echo "case 7b — a groom-ask bead (already escalated to the operator) is excluded (sp-recur-unclaimable)"
# sp-4bjrg carried only groom-asked, no scope/partition label, and was re-flagged on every
# sentinel pass — 62 times on one incident — because the groomer's own escalation label was
# not in this exclusion set. The negative control mirrors it exactly except for the label,
# and must still be reported: the fix is an exclusion, not the check going blind.
out="$(classify "[$(bead sp-unc7c "$GROOM_ASK")]")"
nowant "groom-ask bead not flagged by unclaimable check"  "sp-unc7c" "$out"
out2="$(classify "[$(bead sp-unc7d "")]")"
want "otherwise-identical bead without the label is still flagged (positive control)" "UNCLAIMABLE sp-unc7d" "$out2"

echo
echo "case 10 — parked partition: bead claimable by a parked fayth is silent (not UNCLAIMABLE)"
# NARROW_PARTS omits ops/groom/maechen from the active roster; FULL_PARTS is still the
# chamber, so a bead in ops's or maechen's partition is WAITING, not UNCLAIMABLE.
out="$(classify "[$(bead sp-unc10a incident), $(bead sp-unc10b maechen-sweep), $(bead sp-unc10c "")]" \
    "$NARROW_PARTS" "$FULL_PARTS")"
nowant "parked ops bead not reported UNCLAIMABLE"     "sp-unc10a"             "$out"
nowant "parked maechen bead not reported UNCLAIMABLE" "sp-unc10b"             "$out"
want   "truly unclaimable bead still reported"        "UNCLAIMABLE sp-unc10c" "$out"

echo
echo "case 12 — a bead already claimable is not flagged whatever else it carries"
# The classifier has no notion of "no-loop" — that exclusion happens at the bd query level
# (--exclude-label), before a bead ever reaches this input. This fixture pins the classifier's
# side of the contract: a claimable bead is silent regardless of its other labels, and a bead
# without a partition is still reported (positive control).
out="$(classify "[$(bead sp-unc12a "plan,no-loop"), $(bead sp-unc12b "")]")"
nowant "claimable-anyway bead not reported UNCLAIMABLE"                       "sp-unc12a"             "$out"
want   "bead without a partition still reported (positive control)"          "UNCLAIMABLE sp-unc12b" "$out"

# ==============================================================================================
# T1 — cycle guard: the classifier must not report on its own UNCLAIMABLE reports.
# ==============================================================================================
# A minimal one-persona chamber isolates the guard from partition-matching noise: neither the
# genuine bead nor its report matches builder's "plan" partition, so both would be UNCLAIMABLE
# on partition grounds alone — the guard must still suppress the report.
CYCLE_PARTS="builder|${SCOPE},plan|spira-poison
"

echo
echo "cycle — control: a genuinely unclaimable bead is still reported"
genuine="$(bead sp-real incident)"
out="$(classify "[$genuine]" "$CYCLE_PARTS" "$CYCLE_PARTS")"
want "control: sp-real is reported" "UNCLAIMABLE sp-real" "$out"

echo
echo "cycle — guard: the detector does not report its own report"
report='{"id":"sp-rep","title":"report","status":"open","labels":["groom","repo:spira","'"$SCOPE"'"],"external_ref":"unclaimable:sp-real"}'
out="$(classify "[$report]" "$CYCLE_PARTS" "$CYCLE_PARTS")"
is "guard: no UNCLAIMABLE output for the report itself" "" "$out"

echo
echo "cycle — both present: exactly one line, about the real bead"
out="$(classify "[$genuine, $report]" "$CYCLE_PARTS" "$CYCLE_PARTS")"
is   "both: exactly one UNCLAIMABLE line" "1" "$(printf '%s\n' "$out" | grep -c '^UNCLAIMABLE' || true)"
want "both: the line names the real bead, not the report" "UNCLAIMABLE sp-real" "$out"
nowant "both: the report is not named" "sp-rep" "$out"

# ==============================================================================================
# T2 — one row over the real bd-ready JSON shape, through detect_unclaimable_ready itself.
# ==============================================================================================
echo
echo "T2 — real bd: mixed queue through detect_unclaimable_ready agrees with the classifier"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-unclaimable
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up unclaimable || { echo "test-unclaimable: could not build fixture database"; exit 1; }

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-real5a","title":"claimable: scope,plan","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-real5b","title":"unclaimable: scope only","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

real_out="$(detect_unclaimable_ready 2>/dev/null)"
nowant "real bd: claimable bead not flagged"  "sp-real5a"              "$real_out"
want   "real bd: unclaimable bead is flagged" "UNCLAIMABLE sp-real5b" "$real_out"

# ==============================================================================================
# T1 — file_unclaimable_incidents against a mock incident.sh. No bd needed: the input is a
# canned UNCLAIMABLE line, exactly the shape detect_unclaimable_ready's stdout already has.
# ==============================================================================================

echo
echo "case 8 — a P1 incident is filed in the Groomer partition"
# Only the Groomer can discharge an UNCLAIMABLE finding: add the scope label, correct the
# fayth:, or close the row. Ops cannot, and must not be the sole recipient.
INC_LOG8="$TMP/inc8.log"; : > "$INC_LOG8"
cat > "$TMP/mock-incident8.sh" <<MOCK
#!/usr/bin/env bash
printf 'LABELS=%s file %s\n' "\${SPIRA_INCIDENT_LABELS:-}" "\$*" >> "$INC_LOG8"
MOCK
chmod +x "$TMP/mock-incident8.sh"

SPIRA_INCIDENT_SH="$TMP/mock-incident8.sh" file_unclaimable_incidents \
    "UNCLAIMABLE sp-unc8a — fayth:ops narrows to ops, but none can claim it: ops"
inc_out="$(cat "$INC_LOG8")"
want "incident filed for unclaimable bead"                 "sp-unc8a"                       "$inc_out"
want "incident title contains UNCLAIMABLE prefix"          "UNCLAIMABLE:"                   "$inc_out"
want "incident filed in Groomer partition (positive ctrl)" "${SPIRA_GROOMER_LABEL:-groom}" "$inc_out"

echo
echo "case 9 — no incident for a claimable bead (negative control: an empty line list)"
# detect_unclaimable_ready never emits a line for a claimable bead, so the negative control
# is an empty input — file_unclaimable_incidents must call incident.sh zero times.
INC_LOG9="$TMP/inc9.log"; : > "$INC_LOG9"
cat > "$TMP/mock-incident9.sh" <<MOCK
#!/usr/bin/env bash
printf 'file %s\n' "\$*" >> "$INC_LOG9"
MOCK
chmod +x "$TMP/mock-incident9.sh"

SPIRA_INCIDENT_SH="$TMP/mock-incident9.sh" file_unclaimable_incidents ""
inc9_out="$(cat "$INC_LOG9")"
is "no incident when there is nothing to report" "" "$inc9_out"

echo
echo "case 11 — no incident filed for a parked bead (never reaches the line list)"
# A parked bead never produces an UNCLAIMABLE line in the first place (case 10); the incident
# filer is line-driven and cannot file for what it was never given.
INC_LOG11="$TMP/inc11.log"; : > "$INC_LOG11"
cat > "$TMP/mock-incident11.sh" <<MOCK
#!/usr/bin/env bash
printf 'file %s\n' "\$*" >> "$INC_LOG11"
MOCK
chmod +x "$TMP/mock-incident11.sh"

SPIRA_INCIDENT_SH="$TMP/mock-incident11.sh" file_unclaimable_incidents ""
inc11_out="$(cat "$INC_LOG11")"
is "no incident for parked bead" "" "$inc11_out"

echo
echo "case 13 — SPIRA_INCIDENT_REPO is home repo, not scope label"
# On a run-only install SPIRA_SCOPE_LABEL differs from SPIRA_HOME_REPO. Filing with the
# scope label produces a bead in a repo with no repo-map entry, which Ops parks on claim,
# turning a routine label fix into a page.
INC_LOG13="$TMP/inc13.log"; : > "$INC_LOG13"
cat > "$TMP/mock-incident13.sh" <<MOCK
#!/usr/bin/env bash
printf 'REPO=%s\n' "\${SPIRA_INCIDENT_REPO:-unset}" >> "$INC_LOG13"
MOCK
chmod +x "$TMP/mock-incident13.sh"

_save_home_repo="${SPIRA_HOME_REPO:-}"
export SPIRA_HOME_REPO="fixture-home-repo"
SPIRA_INCIDENT_SH="$TMP/mock-incident13.sh" file_unclaimable_incidents \
    "UNCLAIMABLE sp-unc13a — spira with no matching partition"
export SPIRA_HOME_REPO="$_save_home_repo"

inc13_out="$(cat "$INC_LOG13")"
want   "incident uses SPIRA_HOME_REPO as repo"          "REPO=fixture-home-repo"      "$inc13_out"
nowant "incident does not use SPIRA_SCOPE_LABEL as repo" "REPO=${SPIRA_SCOPE_LABEL}" "$inc13_out"

tl_summary
