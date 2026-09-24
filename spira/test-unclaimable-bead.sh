#!/usr/bin/env bash
#
# test-unclaimable-bead.sh — detect_unclaimable_ready surfaces ready beads no persona can claim
#
# THE DEFECT. sp-f8vry was closed with the detection explicitly not built: "PERMANENT FIX
# NEEDED: Add detection in sentinel CHECK 7 to identify ready beads with no claimable
# persona." The commit it cited (c87909a) asserted only "bead created successfully" — a
# tautology. Seven P1 beads carried fayth:ops on spira,plan labels for fifteen hours while
# the sentinel reported every partition empty, truthfully, on every pass.
#
# THE FIX. detect_unclaimable_ready (lib.sh) reads all ready beads without a partition
# filter, tests each against the full chamber using the same claimers() arithmetic as
# bead.sh, and emits UNCLAIMABLE for any with an empty intersection. It distinguishes an
# idle queue (no beads) from a blocked one (beads present, none claimable).
#
# TWO FAILURE MODES:
#   1. fayth:<persona> on partition labels that persona does not own — the fifteen-hour
#      strand: builder matches the labels, is excluded by the preference; the named persona
#      (ops) is excluded by its own partition. Intersection: empty.
#   2. spira with no partition label — a bead invisible to every persona by construction.
#
# BEFORE THIS FIX, running this suite printed "0 passed, 1 failed" because bd create
# rejected --status (unknown flag) and the test never reached any real assertion.
# After: all cases pass.
#
# defect: sp-9zyu1 sp-f8vry
# covers: spira/lib.sh spira/sentinel.sh
# hermetic-ok: uses a fixture database, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
has()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lacks(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-unclaimable-bead
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up unclaimable || { echo "test-unclaimable-bead: could not build fixture database"; exit 1; }

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such.conf"
acted=0; progressed=0
act()      { acted=$((acted+1)); }
progress() { progressed=$((progressed+1)); act "$@"; }
log()      { : ; }
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-unclaimable-bead.sh"

# ==========================================================================================
echo
echo "case 1 — positive control: a claimable bead (builder: spira,plan) is NOT flagged"
# ==========================================================================================
# Without this, a detect-everything implementation reads as correct. A bead builder can
# claim must pass through detect_unclaimable_ready without producing a UNCLAIMABLE line.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc1a","title":"claimable builder bead","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(detect_unclaimable_ready 2>/dev/null)"
lacks "claimable builder bead is not flagged" "sp-unc1a" "$out"

# ==========================================================================================
echo
echo "case 2 — fayth:ops on spira,plan labels is UNCLAIMABLE (the fifteen-hour strand)"
# ==========================================================================================
# builder matches the partition but is excluded by fayth:ops; ops is excluded by its own
# partition (needs spira,incident, not spira,plan). The sentinel reported every partition
# empty for fifteen hours; this check names the bead and why.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc2a","title":"unclaimable fayth:ops on plan labels","status":"open","issue_type":"task","labels":["fayth:ops","plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(detect_unclaimable_ready 2>/dev/null)"
has  "fayth:ops on spira,plan: UNCLAIMABLE line emitted" "UNCLAIMABLE sp-unc2a" "$out"
has  "fayth:ops on spira,plan: names the preference"      "fayth:ops"            "$out"
has  "fayth:ops on spira,plan: names the rejection"       "ops"                  "$out"

# ==========================================================================================
echo
echo "case 3 — spira with no partition label is UNCLAIMABLE (the sp-bvo7 route)"
# ==========================================================================================
# No persona's partition (spira,plan; spira,incident; ...) is a subset of {spira,repo:spira}.
# Every persona reports 0, every report is truthful. This check distinguishes it from idle.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc3a","title":"unclaimable no partition label","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(detect_unclaimable_ready 2>/dev/null)"
has  "spira no partition: UNCLAIMABLE line emitted"   "UNCLAIMABLE sp-unc3a" "$out"
has  "spira no partition: names a partition to add"   "plan"                 "$out"

# ==========================================================================================
echo
echo "case 4 — idle queue: no beads at all returns no UNCLAIMABLE lines (not a false alarm)"
# ==========================================================================================
# An empty queue and a blocked one both look like 'nothing ready' to CHECK 7. This check
# must be silent when the queue is genuinely empty.
testdb_reset

out="$(detect_unclaimable_ready 2>/dev/null)"
is "empty queue: no UNCLAIMABLE output" "" "$out"

# ==========================================================================================
echo
echo "case 5 — mixed queue: unclaimable bead flagged, claimable neighbour is not"
# ==========================================================================================
# The two beads sit in the same ready set. Only the unclaimable one may appear in output.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc5a","title":"claimable: spira,plan","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-unc5b","title":"unclaimable: spira only","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(detect_unclaimable_ready 2>/dev/null)"
lacks "mixed queue: claimable bead not flagged"     "sp-unc5a" "$out"
has   "mixed queue: unclaimable bead is flagged"    "UNCLAIMABLE sp-unc5b" "$out"

# ==========================================================================================
echo
echo "case 6 — no-scope-label bead is UNCLAIMABLE; in-scope+unclaimable is also not"
# ==========================================================================================
# detect_unclaimable_ready reads the unfiltered ready set (no scope label filter), so a
# bead missing the scope label is seen and reported. Any open bead the loop cannot claim —
# including one missing the scope label — is the condition the detector exists to name.
#
# Positive control (acceptance criterion): a bead that DOES carry the scope label but has
# no partition label is still reported UNCLAIMABLE. Both cases must be asserted together;
# a change is only correct if both produce UNCLAIMABLE output.
testdb_reset
testdb_seed <<JSONL
{"id":"pd-unc6a","title":"no scope label: unclaimable by any persona","status":"open","issue_type":"task","labels":["plan","repo:pokedumpster"]}
{"id":"sp-unc6c","title":"in-scope unclaimable: spira, no partition","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(detect_unclaimable_ready 2>/dev/null)"
has   "no-scope-label bead reported UNCLAIMABLE (missing scope label)" "UNCLAIMABLE pd-unc6a" "$out"
has   "in-scope no-partition bead IS reported (positive control)"      "UNCLAIMABLE sp-unc6c" "$out"

# ==========================================================================================
echo
echo "case 7 — spira-poison and needs-ryan beads are excluded (have their own check)"
# ==========================================================================================
# detect_unclaimable_ready must not flag beads already handled by CHECK 4 (poison) or the
# ask taxonomy (needs-ryan). Both would otherwise produce noise on every pass.
testdb_reset
# THE SEEDED LABEL MUST MATCH SPIRA_ASK_LABEL. detect_unclaimable_ready excludes
# beads whose label set intersects {SPIRA_ASK_LABEL, spira-poison}. A bead carrying
# a hardcoded "needs-ryan" is invisible to the exclusion when SPIRA_ASK_LABEL is
# "needs-operator" (conf.sh default), so the bead appears UNCLAIMABLE instead of
# being skipped — and the assertion fails. Use the live variable.
{ cat <<JSONL
{"id":"sp-unc6a","title":"poisoned: skipped","status":"open","issue_type":"task","labels":["spira-poison","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL
printf '{"id":"sp-unc6b","title":"ask-label: skipped","status":"open","issue_type":"task","labels":["%s","repo:spira","%s"]}\n' \
    "${SPIRA_ASK_LABEL:-needs-operator}" "${SPIRA_SCOPE_LABEL}"
} | testdb_seed

out="$(detect_unclaimable_ready 2>/dev/null)"
lacks "poisoned bead not flagged by unclaimable check"   "sp-unc6a" "$out"
lacks "needs-ryan bead not flagged by unclaimable check" "sp-unc6b" "$out"

# ==========================================================================================
echo
echo "case 8 — file_unclaimable_incidents: a P1 incident is filed in the Groomer partition"
# ==========================================================================================
# file_unclaimable_incidents must file into the Groomer partition (SPIRA_GROOMER_LABEL) so
# the Groomer — not Ops — receives the work. Only the Groomer can discharge it: it can add
# the scope label, correct the fayth:, or close the row. Ops cannot; it is the diagnostician
# but not the terminus. The mock captures SPIRA_INCIDENT_LABELS so the partition is verifiable.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc8a","title":"unclaimable: fayth:ops on plan labels","status":"open","issue_type":"task","labels":["fayth:ops","plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

# The mock writes its env and args to a file whose path is embedded at write time.
INC_LOG8="$TMP/inc8.log"
: > "$INC_LOG8"
cat > "$TMP/mock-incident8.sh" <<MOCK
#!/usr/bin/env bash
printf 'LABELS=%s file %s\n' "\${SPIRA_INCIDENT_LABELS:-}" "\$*" >> "$INC_LOG8"
MOCK
chmod +x "$TMP/mock-incident8.sh"

unc="$(detect_unclaimable_ready 2>/dev/null)"
SPIRA_INCIDENT_SH="$TMP/mock-incident8.sh" file_unclaimable_incidents "$unc"
inc_out="$(cat "$INC_LOG8")"
has  "incident filed for unclaimable bead"                "sp-unc8a"                       "$inc_out"
has  "incident title contains UNCLAIMABLE prefix"         "UNCLAIMABLE:"                   "$inc_out"
has  "incident filed in Groomer partition (positive ctrl)" "${SPIRA_GROOMER_LABEL:-groom}" "$inc_out"

# ==========================================================================================
echo
echo "case 9 — file_unclaimable_incidents: no incident for a claimable bead (negative control)"
# ==========================================================================================
# A claimable bead must never produce an incident call. Without this, a detect-everything
# implementation files noise on every pass.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc9a","title":"claimable: spira,plan","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

INC_LOG9="$TMP/inc9.log"
: > "$INC_LOG9"
cat > "$TMP/mock-incident9.sh" <<MOCK
#!/usr/bin/env bash
printf 'file %s\n' "\$*" >> "$INC_LOG9"
MOCK
chmod +x "$TMP/mock-incident9.sh"

unc="$(detect_unclaimable_ready 2>/dev/null)"
SPIRA_INCIDENT_SH="$TMP/mock-incident9.sh" file_unclaimable_incidents "$unc"
inc9_out="$(cat "$INC_LOG9")"
is "no incident for claimable bead" "" "$inc9_out"

# ==========================================================================================
echo
echo "case 10 — parked partition: bead claimable by a parked fayth is silent (not UNCLAIMABLE)"
# ==========================================================================================
# When SPIRA_FAYTHS narrows the active roster (e.g., builder only), beads whose partition
# belongs to a parked persona (ops, groomer, maechen) are WAITING — the operator narrowed
# the roster deliberately. They must not produce UNCLAIMABLE output or file incidents.
# That advice ("add one of: plan, spike") would relabel the bead into the wrong partition.
#
# POSITIVE CONTROL: a truly unclaimable bead (no partition label) still fires.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc10a","title":"parked: ops partition (spira,incident)","status":"open","issue_type":"task","labels":["incident","repo:spira","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-unc10b","title":"parked: maechen partition (spira,maechen-sweep)","status":"open","issue_type":"task","labels":["maechen-sweep","repo:spira","${SPIRA_SCOPE_LABEL}"]}
{"id":"sp-unc10c","title":"truly unclaimable: spira only, no partition","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(SPIRA_FAYTHS="builder spike" detect_unclaimable_ready 2>/dev/null)"
lacks "parked ops bead not reported UNCLAIMABLE"    "sp-unc10a" "$out"
lacks "parked maechen bead not reported UNCLAIMABLE" "sp-unc10b" "$out"
has   "truly unclaimable bead still reported"       "UNCLAIMABLE sp-unc10c" "$out"

# ==========================================================================================
echo
echo "case 11 — parked partition: no incident filed for a parked bead"
# ==========================================================================================
# file_unclaimable_incidents must be silent for a parked bead. An incident relabelled into
# the wrong partition is claimed by the wrong persona and that advice does damage.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc11a","title":"parked: ops partition","status":"open","issue_type":"task","labels":["incident","repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

INC_LOG11="$TMP/inc11.log"
: > "$INC_LOG11"
cat > "$TMP/mock-incident11.sh" <<MOCK
#!/usr/bin/env bash
printf 'file %s\n' "\$*" >> "$INC_LOG11"
MOCK
chmod +x "$TMP/mock-incident11.sh"

unc="$(SPIRA_FAYTHS="builder spike" detect_unclaimable_ready 2>/dev/null)"
SPIRA_INCIDENT_SH="$TMP/mock-incident11.sh" file_unclaimable_incidents "$unc"
inc11_out="$(cat "$INC_LOG11")"
is "no incident for parked bead" "" "$inc11_out"

# ==========================================================================================
echo
echo "case 12 — no-loop: a bead marked no-loop is NOT reported by detect_unclaimable_ready"
# ==========================================================================================
# A bead carrying the no-loop label is intentionally unclaimable — an operator plan
# recorded for the record, not for the loop. READY_ARGS excludes it via --exclude-label,
# so detect_unclaimable_ready never sees it and never files a remedy to make it claimable.
#
# POSITIVE CONTROL: a bead without no-loop (and with no partition) still fires.
# Without this, a scanner that never checks anything reads as correct.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-unc12a","title":"no-loop: intentionally unclaimable","status":"open","issue_type":"task","labels":["plan","repo:spira","${SPIRA_SCOPE_LABEL}","${SPIRA_NO_LOOP_LABEL:-no-loop}"]}
{"id":"sp-unc12b","title":"no-loop absent: unclaimable (no partition)","status":"open","issue_type":"task","labels":["repo:spira","${SPIRA_SCOPE_LABEL}"]}
JSONL

out="$(detect_unclaimable_ready 2>/dev/null)"
lacks "no-loop bead not reported UNCLAIMABLE (positive control fires below)" \
    "sp-unc12a" "$out"
has   "bead without no-loop still reported when unclaimable (positive control)" \
    "UNCLAIMABLE sp-unc12b" "$out"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
