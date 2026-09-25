#!/usr/bin/env bash
#
# test-fayth.sh — the persona roster and predicates: discovery, own-partition selection,
#   fayth_exclude, effective lanes, and the fields the shipped fayths must carry.
#
#   ./test-fayth.sh
#
# T1 for dispatch UC-07/08/13. Merges test-fayth-predicates.sh's runtime rows (the exact
# source-text rows are deleted: the runtime override rows below already prove the property
# that a predicate is built from a variable, not a literal), test-lanes.sh's roster-split
# and real-roster rows, all of test-effective-lanes.sh, and test-spike.sh's fayth-fields row.
#
# THE DEFECT (sp-fayth-predicate). sentinel.sh CHECK 7 computed one $ready from the
# builder's own predicate and gated every persona's summon on it, so ops never woke on its
# own work. summon_fayth now asks fayth_ready, which sources the fayth file and asks
# ready_count through THAT fayth's FAYTH_LABELS. spira_fayths enumerates chamber/*.fayth
# rather than returning a literal name, so installing a fayth is the whole registration.
#
# WHAT THIS SUITE DOES NOT USE. No bd, no systemd, no network for the library-level rows;
# ready_count and SPIRA_SUMMON are stubs. Section-local env overrides are restored (and
# lib.sh re-sourced) before the next section relies on the real chamber again.
#
# defect: sp-fayth-predicate sp-czsf4 sp-xrkuu
# covers: spira/lib.sh spira/sentinel.sh spira/schema.sh spira/conf.sh spira/doctor.sh spira/chamber/*.fayth UC-dispatch-07 UC-dispatch-08 UC-dispatch-13
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# Run with the real chamber so the suite exercises the installed fayths, not a model of
# them. SPIRA_CONF at a nonexistent path so no host config leaks verdicts into the suite
# (law-gates-run-in-a-clean-environment).
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# Labels file: ready_count writes here from inside a subshell (fayth_ready's (...) call)
# so the parent can observe which predicate was asked. A subshell variable assignment
# cannot reach the parent, so the file is the channel.
LABELS_FILE="$T/observed-labels"

# Override ready_count AFTER sourcing lib.sh. bash subshells inherit the calling shell's
# functions, so fayth_ready — which invokes ready_count inside a (...) subshell — picks
# this override up rather than the real one.
MOCK_READY=0
ready_count() {
    printf '%s' "$1" > "$LABELS_FILE"
    printf '%d' "$MOCK_READY"
}

# Stub aeon_count so no running aeons are reported: fayth_free would otherwise see a
# live slot and return 0, blocking the summon path before ready_count is called.
aeon_count() { printf '0'; }

# Stub SPIRA_SUMMON so summon_fayth does not attempt to start a systemd unit.
MOCK_SUMMON="$T/summon"
printf '#!/bin/sh\nexit 0\n' > "$MOCK_SUMMON"
chmod +x "$MOCK_SUMMON"
export SPIRA_SUMMON="$MOCK_SUMMON"

# ==========================================================================================
# spira_fayths — discovers every fayth in the chamber, not just one
# ==========================================================================================
unset SPIRA_FAYTHS 2>/dev/null || true
got="$(spira_fayths)"
# The real chamber has builder and ops at minimum; both must appear.
want "builder appears in roster" "builder" "$got"
want "ops appears in roster"     "ops"     "$got"

# ==========================================================================================
# ops.fayth — its FAYTH_LABELS is its OWN partition, not the builder's
# ==========================================================================================
# A POSITIVE CONTROL AGAINST THE REAL FILE. The defect was that ops.fayth carried a correct
# predicate that the harness never used; this assertion fails if the file is ever edited to
# carry the builder's predicate, or if fayth_get resolves the wrong file.
ops_labels="$(fayth_get ops FAYTH_LABELS)"
nowant "ops FAYTH_LABELS is not the builder's spira,plan" "spira,plan" " $ops_labels "
want "ops FAYTH_LABELS contains its own partition"     "incident"   "$ops_labels"

# ==========================================================================================
# summon_fayth — ops uses ITS OWN predicate, not the builder's
# ==========================================================================================
MOCK_READY=1; rm -f "$LABELS_FILE"
summon_fayth ops >/dev/null 2>&1 || true
observed="$(cat "$LABELS_FILE" 2>/dev/null)"
nowant "ops did NOT ask for spira,plan beads" "spira,plan" " $observed "
want "ops asked for its own incident beads" "incident"   "$observed"

# ==========================================================================================
# summon_fayth — builder uses ITS OWN predicate
# ==========================================================================================
MOCK_READY=1; rm -f "$LABELS_FILE"
summon_fayth builder >/dev/null 2>&1 || true
observed="$(cat "$LABELS_FILE" 2>/dev/null)"
is "builder asked for its own partition beads" \
   "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan}" "$observed"

# ==========================================================================================
# positive control — old single-predicate approach misses ops work
# ==========================================================================================
# THE OLD CODE IN THREE LINES:
#   ready="$(ready_count spira,plan ...)"   # hardcoded builder predicate
#   if [ "$ready" -gt 0 ]; then             # gates ALL personas, including ops
#       for f in $FAYTHS; do summon_fayth "$f"; done
#   fi
MOCK_READY=0
plan_ready="$(ready_count "spira,plan" "")"
is "old code: plan_ready=0 (no plan work)" "0" "$plan_ready"

MOCK_READY=1; rm -f "$LABELS_FILE"
summon_fayth ops >/dev/null 2>&1 || true
observed="$(cat "$LABELS_FILE" 2>/dev/null)"
want "ops predicate was asked even when plan_ready=0" "incident" "$observed"

# ==========================================================================================
# summon_fayth — SPIRA_AEON_CPU_QUOTA is passed to the summon command
# ==========================================================================================
ARGS_FILE="$T/summon-args"
MOCK_QUOTA="$T/summon-quota"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\nexit 0\n' "$ARGS_FILE" > "$MOCK_QUOTA"
chmod +x "$MOCK_QUOTA"
export SPIRA_SUMMON="$MOCK_QUOTA"

MOCK_READY=1; rm -f "$ARGS_FILE" "$LABELS_FILE"
unset SPIRA_AEON_CPU_QUOTA 2>/dev/null || true
summon_fayth ops >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want "default quota: CPUQuota=70% appears in args" "CPUQuota=70%" "$args"

MOCK_READY=1; rm -f "$ARGS_FILE" "$LABELS_FILE"
export SPIRA_AEON_CPU_QUOTA=90%
summon_fayth ops >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want "custom quota: CPUQuota=90% appears in args"    "CPUQuota=90%" "$args"
nowant "custom quota: CPUQuota=70% is absent from args" "CPUQuota=70%" "$args"

export SPIRA_SUMMON="$MOCK_SUMMON"
unset SPIRA_AEON_CPU_QUOTA

# ==========================================================================================
# summon_fayth — an express grant passes the label to the claim predicate (sp-zcvh1)
# ==========================================================================================
# summon_fayth's third argument is carried to the aeon as SPIRA_REQUIRE_LABEL, so the aeon
# started under an express grant cannot claim a non-express bead.
export SPIRA_SUMMON="$MOCK_QUOTA"

MOCK_READY=1; rm -f "$ARGS_FILE" "$LABELS_FILE"
summon_fayth builder 1 >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
nowant "no require-label: SPIRA_REQUIRE_LABEL is absent from args" "SPIRA_REQUIRE_LABEL" "$args"

MOCK_READY=1; rm -f "$ARGS_FILE" "$LABELS_FILE"
summon_fayth builder 1 express >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want "express grant: SPIRA_REQUIRE_LABEL=express appears in args" \
     "SPIRA_REQUIRE_LABEL=express" "$args"

export SPIRA_SUMMON="$MOCK_SUMMON"

# ==========================================================================================
# D5 — spira_lane_fayths / spira_task_fayths: the roster split (from test-lanes.sh)
# ==========================================================================================
# A synthetic chamber, neither fayth named "builder" or "ops" — the rule must be bound to
# the FAYTH_LANE declaration, not to a hardcoded name.
LANES_HOME="$T/lanes-home"
mkdir -p "$LANES_HOME/chamber"
cat > "$LANES_HOME/chamber/worker.fayth" <<'F'
FAYTH_NAME=worker
FAYTH_LABELS="spira,plan"
FAYTH_MAX_CONCURRENT=2
FAYTH_ELASTIC=1
F
cat > "$LANES_HOME/chamber/guardian.fayth" <<'F'
FAYTH_NAME=guardian
FAYTH_LABELS="spira,incident"
FAYTH_MAX_CONCURRENT=1
FAYTH_LANE=priority
F

export SPIRA_HOME="$LANES_HOME"
export SPIRA_FAYTHS="worker guardian"

lane="$(spira_lane_fayths)"
task="$(spira_task_fayths)"

is "spira_lane_fayths returns the FAYTH_LANE fayth"      "guardian" "$lane"
is "spira_task_fayths excludes the FAYTH_LANE fayth"     "worker"   "$task"
nowant "the lane fayth does not appear in task fayths"   "guardian" "$task"
nowant "the task fayth does not appear in lane fayths"   "worker"   "$lane"

# POSITIVE CONTROL: both functions return something, so absence above is the exclusion
# working and not both functions returning empty.
is "there is at least one lane fayth"  "1" "$([ -n "$lane" ] && echo 1 || echo 0)"
is "there is at least one task fayth"  "1" "$([ -n "$task" ] && echo 1 || echo 0)"

# ==========================================================================================
# D5 — the real roster: ops is a lane fayth, builder is a task fayth (from test-lanes.sh)
# ==========================================================================================
export SPIRA_HOME="$HERE"
export SPIRA_FAYTHS="builder ops"

real_task="$(spira_task_fayths)"
real_lane="$(spira_lane_fayths)"

want   "ops appears in lane fayths" "ops" "$real_lane"
nowant "ops does NOT appear in task fayths" "ops" "$real_task"
want   "builder is still in the task pool" "builder" "$real_task"

unset SPIRA_FAYTHS

# ==========================================================================================
# T0 — no active fayth carries a bare partition literal (from test-fayth-predicates.sh)
# ==========================================================================================
# A FAYTH_LABELS line with a bare partition literal ends with }word" — where the character
# immediately after } is a lowercase letter (variable references end with }$VARNAME"). The
# exact-string checks that used to sit here (builder built from $SPIRA_PLAN_LABEL, ops from
# $SPIRA_INCIDENT_LABEL, and their negations) are deleted: the runtime override rows above
# already prove each persona asks through its own configured label, which is the property
# those source-text checks existed to protect.
BARE_PATTERN='}[a-z][a-z_-]*"'

# POSITIVE CONTROL: plant an offender and prove the grep fires on it first
# (law-absence-needs-a-positive-control).
mkdir -p "$T/bare-scan"
printf 'FAYTH_NAME=offender\nFAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan"\n' \
    > "$T/bare-scan/offender.fayth"
if grep -qE "FAYTH_LABELS=.*$BARE_PATTERN" "$T/bare-scan/offender.fayth" 2>/dev/null; then
    ok "positive control: bare-literal scanner fires on a planted offender"
else
    bad "positive control: bare-literal scanner DID NOT fire on a planted offender" \
        "the check is broken — cannot trust its silence"
fi

for f in "$HERE/chamber/"*.fayth; do
    [ -e "$f" ] || continue
    name="${f##*/}"; name="${name%.fayth}"
    # Comment lines are excluded — a comment about a literal is not a literal.
    noncomment="$(grep -v '^[[:space:]]*#' "$f" 2>/dev/null)"
    if printf '%s\n' "$noncomment" | grep -qE "FAYTH_LABELS=.*$BARE_PATTERN"; then
        bad "$name.fayth: FAYTH_LABELS contains a bare partition literal" \
            "$(printf '%s\n' "$noncomment" | grep 'FAYTH_LABELS=')"
    else
        ok "$name.fayth: FAYTH_LABELS resolves through a variable, not a bare literal"
    fi
done

# ==========================================================================================
# UC-dispatch-13 — effective lanes: .spira/modes ∩ repo-map lanes ∩ SPIRA_FAYTHS
# (all of test-effective-lanes.sh; ACCEPTANCE CRITERIA from bead sp-czsf4)
# ==========================================================================================
#   1. modes=self, map=develop  -> maechen-sweep refused, diagnostic names repo-map.
#   2. modes=consume, map=self  -> plan alone; refused lanes named .spira/modes.
#   3. SPIRA_FAYTHS missing a fayth -> that lane absent; diagnostic names SPIRA_FAYTHS.
#   4. No .spira/modes, no lanes column -> plan alone, no refusal diagnostics.
#   5. Missing working copy -> no crash; .spira/modes treated as absent.
#   6. .spira/modes parse error -> modes-error line; effective falls back to plan.
#
# POSITIVE CONTROLS. Each absence assertion is preceded by a presence assertion so
# a check pointed at the wrong thing and a check that found nothing look different
# (law-absence-needs-a-positive-control).
EL="$T/el"
mkdir -p "$EL/run" "$EL/chamber"

# Non-default label values where possible so assertions are not trivially satisfied
# by literals in the code (law-gates-run-in-a-clean-environment).
export SPIRA_RUN="$EL/run"
export SPIRA_CONF="$EL/no-such.conf"
export SPIRA_HOME="$EL"
export SPIRA_DB="$EL/no-db"
export SPIRA_PLAN_LABEL="plan"
export SPIRA_INCIDENT_LABEL="incident"
export SPIRA_GROOMER_LABEL="groom"
export SPIRA_MAECHEN_LABEL="maechen-sweep"
export SPIRA_SPIKE_LABEL="spike"
export SPIRA_CZAR_LABEL="czar-trigger"

EL_MAP="$EL/repo-map"
export SPIRA_REPO_MAP="$EL_MAP"

# Minimal chamber — each fayth exposes only the one label it owns.
for _f_name in builder ops groomer maechen spike czar; do
    case "$_f_name" in
        builder) _f_label='${SPIRA_PLAN_LABEL:-plan}' ;;
        ops)     _f_label='${SPIRA_INCIDENT_LABEL:-incident}' ;;
        groomer) _f_label='${SPIRA_GROOMER_LABEL:-groom}' ;;
        maechen) _f_label='${SPIRA_MAECHEN_LABEL:-maechen-sweep}' ;;
        spike)   _f_label='${SPIRA_SPIKE_LABEL:-spike}' ;;
        czar)    _f_label='${SPIRA_CZAR_LABEL:-czar-trigger}' ;;
    esac
    printf 'FAYTH_LABELS="%s"\n' "$_f_label" > "$EL/chamber/$_f_name.fayth"
done
unset _f_name _f_label

# shellcheck disable=SC1090
. "$HERE/lib.sh"

EL_REPO="$EL/repo"
mkdir -p "$EL_REPO/.spira"

# criterion 1 — modes=self, map=develop: maechen-sweep refused by repo-map
printf 'alpha | %s | push | origin/main | | true | develop\n' "$EL_REPO" > "$EL_MAP"
printf 'self\n' > "$EL_REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: a lane that IS in both is present in effective.
out1="$(_spira_lane_diag alpha "$EL_REPO")"
want "crit1 pos: plan in effective"        "effective:" "$out1"
want "crit1 pos: plan present"             "plan"       "$out1"
want "crit1 pos: incident present"         "plan incident" "$out1"

# maechen-sweep: in modes (self) but NOT in map (develop) -> refused by repo-map
want  "crit1: maechen-sweep refused"       "refused: maechen-sweep"   "$out1"
want  "crit1: refuser is repo-map"         "refused: maechen-sweep by repo-map" "$out1"
# czar-trigger also not in develop
want  "crit1: czar-trigger refused"        "refused: czar-trigger by repo-map" "$out1"
# plan, incident, groom, spike are in effective (develop includes them)
nowant "crit1: plan not refused"           "refused: plan"            "$out1"
nowant "crit1: incident not refused"       "refused: incident"        "$out1"
nowant "crit1: groom not refused"          "refused: groom"           "$out1"
nowant "crit1: spike not refused"          "refused: spike"           "$out1"
# fayths and modes do NOT appear as refusers for maechen-sweep
nowant "crit1: SPIRA_FAYTHS not named"    "maechen-sweep by SPIRA_FAYTHS" "$out1"
nowant "crit1: modes not named"           "maechen-sweep by .spira/modes" "$out1"

rm -f "$EL_REPO/.spira/modes"

# criterion 2 — modes=consume, map=self: plan alone; refused lanes name .spira/modes
printf 'alpha | %s | push | origin/main | | true | self\n' "$EL_REPO" > "$EL_MAP"
printf 'consume\n' > "$EL_REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: plan is in effective.
out2="$(_spira_lane_diag alpha "$EL_REPO")"
want "crit2 pos: effective contains plan"  "effective: plan"          "$out2"

# All non-plan lanes are refused by .spira/modes
want  "crit2: incident refused by modes"  "refused: incident by .spira/modes"     "$out2"
want  "crit2: groom refused by modes"     "refused: groom by .spira/modes"        "$out2"
want  "crit2: maechen refused by modes"   "refused: maechen-sweep by .spira/modes" "$out2"
want  "crit2: spike refused by modes"     "refused: spike by .spira/modes"        "$out2"
want  "crit2: czar refused by modes"      "refused: czar-trigger by .spira/modes" "$out2"
# repo-map and SPIRA_FAYTHS do NOT appear as refusers (they grant all)
nowant "crit2: repo-map not named"        "by repo-map"              "$out2"
nowant "crit2: SPIRA_FAYTHS not named"   "by SPIRA_FAYTHS"          "$out2"

rm -f "$EL_REPO/.spira/modes"

# criterion 3 — SPIRA_FAYTHS missing maechen: maechen-sweep refused by SPIRA_FAYTHS
printf 'alpha | %s | push | origin/main | | true | self\n' "$EL_REPO" > "$EL_MAP"
printf 'self\n' > "$EL_REPO/.spira/modes"
# Roster excludes maechen — no maechen.fayth needed, SPIRA_FAYTHS is what matters.
export SPIRA_FAYTHS="builder ops groomer spike czar"

# POSITIVE CONTROL: a lane whose fayth IS configured is in effective.
out3="$(_spira_lane_diag alpha "$EL_REPO")"
want "crit3 pos: plan in effective"       "plan"                      "$out3"
want "crit3 pos: incident in effective"   "incident"                  "$out3"
# THE FIX: this was `nowant ... "effective:.*maechen-sweep" ...` — a glob-looking string
# that `nowant`'s plain substring match never finds literally, so it passed regardless of
# what the code under test did. Assert against the actual effective: line instead.
effective_line="$(printf '%s\n' "$out3" | grep '^effective:')"
nowant "crit3 pos: maechen NOT effective" "maechen-sweep" "$effective_line"

# maechen-sweep: in both map and modes but fayths doesn't include it
want  "crit3: maechen refused"            "refused: maechen-sweep"    "$out3"
want  "crit3: refuser is SPIRA_FAYTHS"   "refused: maechen-sweep by SPIRA_FAYTHS" "$out3"
nowant "crit3: repo-map not named"        "maechen-sweep by repo-map" "$out3"
nowant "crit3: modes not named"           "maechen-sweep by .spira/modes" "$out3"

rm -f "$EL_REPO/.spira/modes"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# criterion 4 — no .spira/modes, no lanes column: plan alone, no refusal lines
printf 'alpha | %s | push | origin/main | | true\n' "$EL_REPO" > "$EL_MAP"
# No .spira/modes file.
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: plan is present in effective (proves the check runs).
out4="$(_spira_lane_diag alpha "$EL_REPO")"
want "crit4 pos: effective line present"  "effective:"                "$out4"
want "crit4 pos: plan in effective"       "plan"                      "$out4"

# No refused lines at all.
nowant "crit4: no refused lines"          "refused:"                  "$out4"

# criterion 5 — missing working copy: no crash, .spira/modes treated as absent
printf 'alpha | /nonexistent/path | push | origin/main | | true | self\n' > "$EL_MAP"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

rc5=0; out5="$(_spira_lane_diag alpha "/nonexistent/path")" || rc5=$?
is    "crit5: exits cleanly"              "0"       "$rc5"
want  "crit5: effective line present"     "effective:" "$out5"
# With no .spira/modes the effective set is map_lanes ∩ fayths_lanes = all-of-self
want  "crit5: plan in effective"          "plan"      "$out5"
# No modes file -> modes is not a refuser for anything map+fayths both grant
nowant "crit5: modes not a refuser"       "by .spira/modes" "$out5"

# criterion 6 — .spira/modes parse error: modes-error line, falls back to plan
printf 'alpha | %s | push | origin/main | | true | self\n' "$EL_REPO" > "$EL_MAP"
export SPIRA_FAYTHS="builder ops groomer maechen spike czar"

# POSITIVE CONTROL: a valid modes file produces no modes-error line.
printf 'self\n' > "$EL_REPO/.spira/modes"
out6_ok="$(_spira_lane_diag alpha "$EL_REPO")"
nowant "crit6 pos: valid modes: no error" "modes-error:" "$out6_ok"

# Invalid modes file produces modes-error line.
printf 'badmode\n' > "$EL_REPO/.spira/modes"
out6="$(_spira_lane_diag alpha "$EL_REPO")"
want  "crit6: modes-error line present"   "modes-error:"   "$out6"
want  "crit6: effective line still present" "effective:"   "$out6"

rm -f "$EL_REPO/.spira/modes"

# ==========================================================================================
# Restore the real chamber for the remaining, chamber-file-level sections
# ==========================================================================================
unset SPIRA_FAYTHS SPIRA_REPO_MAP SPIRA_DB
unset SPIRA_PLAN_LABEL SPIRA_INCIDENT_LABEL SPIRA_GROOMER_LABEL
unset SPIRA_MAECHEN_LABEL SPIRA_SPIKE_LABEL SPIRA_CZAR_LABEL
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh" 2>/dev/null

# ==========================================================================================
# UC-dispatch-08 — every summonable persona resolves to a non-empty predicate at runtime
# (from test-fayth-predicates.sh)
# ==========================================================================================
# fayth_get sources the fayth in a subshell with conf.sh in scope, so $SPIRA_PLAN_LABEL
# expands to its configured value. An unset or empty label would produce an empty
# FAYTH_LABELS, which the cockpit renders as ? (correct) but the sentinel would see as a
# malformed predicate.
#
# "SUMMONABLE" EXCLUDES AN OPERATOR PERSONA: the concierge claims nothing and runs for days
# as the operator's own session, so a partition would be actively wrong. The exclusion is
# keyed on FAYTH_SUMMON rather than on the name, because a check that recognised
# "concierge" by string would bind the wrong persona the day it is renamed.
_checked=0
for f in $(fayth_names 2>/dev/null); do
    if [ "$(fayth_get "$f" FAYTH_SUMMON auto 2>/dev/null)" != auto ]; then
        ok "$f: operator persona, no predicate expected"
        continue
    fi
    _checked=$(( _checked + 1 ))
    labels="$(fayth_get "$f" FAYTH_LABELS '' 2>/dev/null)"
    if [ -n "$labels" ]; then
        ok "$f: FAYTH_LABELS resolves to non-empty: $labels"
    else
        bad "$f: FAYTH_LABELS resolved to empty" "predicate is missing"
    fi
done

# AND THE EXCLUSION MUST NOT HAVE EMPTIED THE LOOP (law-absence-needs-a-positive-control).
if [ "$_checked" -gt 0 ]; then
    ok "$_checked summonable persona(s) were actually checked"
else
    bad "the predicate loop checked nothing" "every fayth was skipped as operator-summoned"
fi

# ==========================================================================================
# REGRESSION (sp-xrkuu class) — schema_name fails closed on undeclared keys
# (from test-fayth-predicates.sh)
# ==========================================================================================
# The defect class: a reader that asks for a partition by name gets back the empty string
# (or an unexpanded variable) and hands it to bd as a label query. bd answers [] truthfully
# and the caller reads "no work" instead of "error". schema_name must exit non-zero on any
# key it does not know, so the error is unmistakable.
SCHEMA_T="$T/no.conf"

# POSITIVE CONTROL: schema_name fails on a made-up key before we check the real ones.
schema_out="$(SPIRA_HOME="$HERE" SPIRA_CONF="$SCHEMA_T" \
    bash "$HERE/schema.sh" name no-such-partition-xyz 2>&1)" && schema_exit=0 || schema_exit=$?

if [ "$schema_exit" -eq 2 ] && [[ "$schema_out" == *"no-such-partition-xyz"* ]]; then
    ok "schema_name: undeclared key exits 2 and names the missing key"
else
    bad "schema_name: undeclared key must exit 2 and name the missing key" \
        "exit=$schema_exit, output='$schema_out'"
fi

# plan and incident must be declared — a partition the fayth uses must be one schema_name
# can hand to a caller by validated name (not just by default fallback in the variable form).
for key in plan incident; do
    label="$(SPIRA_HOME="$HERE" SPIRA_CONF="$SCHEMA_T" \
        bash "$HERE/schema.sh" name "$key" 2>/dev/null)" && key_exit=0 || key_exit=$?
    if [ "$key_exit" -eq 0 ] && [ -n "$label" ]; then
        ok "schema_name '$key' is declared, resolves to: $label"
    else
        bad "schema_name '$key' is NOT declared in schema.sh" \
            "exit=$key_exit, output='$label'"
    fi
done

# DISCRIMINATING: a custom SPIRA_PLAN_LABEL must propagate through schema_name.
custom_label="$(SPIRA_HOME="$HERE" SPIRA_CONF="$SCHEMA_T" \
    SPIRA_PLAN_LABEL=work bash "$HERE/schema.sh" name plan 2>/dev/null)"
is "schema_name plan: SPIRA_PLAN_LABEL=work propagates to 'work', not 'plan'" \
   "work" "$custom_label"

# ==========================================================================================
# D4 — the spike fayth's own fields (from test-spike.sh)
# ==========================================================================================
# The predicate is built from the configured label rather than a literal, which is the
# property that keeps the fayth, the brief and confine.sh agreeing.
fayth_src="$(cat "$HERE/chamber/spike.fayth")"
want "spike: the predicate is built from the configured label" \
     'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_SPIKE_LABEL"' "$fayth_src"
nowant "spike: and does not hardcode one" 'FAYTH_LABELS="spira,spike"' "$fayth_src"
want "spike: a spike may search the web" "WebSearch" "$fayth_src"
want "spike: and it may build"           "Bash"      "$fayth_src"
want "spike: and edit"                   "Edit"      "$fayth_src"

# ==========================================================================================
# G16 — fayth_exclude adds every OTHER persona's fayth:<name> label and the queue-wait label
# ==========================================================================================
# lib.sh l.988: today fayth_exclude is exercised only indirectly, through the detectors that
# call fayth_ready/summon_fayth. A direct row proves the exclusion set itself: every OTHER
# persona's fayth:<name>, plus $SPIRA_QUEUE_WAIT_LABEL, never the caller's own name.
export SPIRA_FAYTHS="builder ops groomer"
export SPIRA_QUEUE_WAIT_LABEL="g16-queue-wait"

excl="$(fayth_exclude ops "ops-own-label")"
want   "G16: the persona's own exclude labels are kept"          "ops-own-label"  "$excl"
want   "G16: SPIRA_QUEUE_WAIT_LABEL is added"                     "g16-queue-wait" "$excl"
want   "G16: excludes another persona's fayth: label (builder)"  "fayth:builder"  "$excl"
want   "G16: excludes another persona's fayth: label (groomer)"  "fayth:groomer"  "$excl"
nowant "G16: does not exclude its own fayth: label (ops)"        "fayth:ops"      "$excl"

unset SPIRA_QUEUE_WAIT_LABEL SPIRA_FAYTHS

tl_summary
