#!/usr/bin/env bash
#
# test-fayth.sh — the persona roster and predicates: discovery, own-partition selection,
#   fayth_exclude, and the fields the shipped fayths must carry.
#
#   ./test-fayth.sh
#
# T1 for dispatch UC-07/08. Merges test-fayth-predicates.sh's runtime rows (the exact
# source-text rows are deleted: the runtime override rows below already prove the property
# that a predicate is built from a variable, not a literal), test-lanes.sh's roster-split
# and real-roster rows, and test-spike.sh's fayth-fields row.
#
# UC-dispatch-13 (effective lanes: .spira/modes ∩ repo-map lanes ∩ SPIRA_FAYTHS, all of the
# former test-effective-lanes.sh) was retired at sp-27hsi along with the dead functions it
# drove (_spira_lane_diag, _spira_modes_lanes, _spira_fayths_lane_set): nothing calls that
# three-way-intersection-with-named-refusal diagnostic any more. Lane admission in the live
# path is maechen-trigger/groom-trigger.sh's much simpler spira_lane_admitted (any repo-map
# lanes column membership, no .spira/modes or SPIRA_FAYTHS intersection, no refusal naming),
# which stays and is exercised through maechen-trigger's own mocked-port unit tests, not a
# bash suite. See docs/test-plan/dispatch.toml for the UC-dispatch-13 [use_case.uncovered].
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
# tier: T1
# covers: spira/lib.sh sentinel/src/* spira/schema.sh spira/conf.sh doctor/src/* spira/chamber/*.fayth UC-dispatch-07 UC-dispatch-08
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# Run with the real chamber so the suite exercises the installed fayths, not a model of
# them. SPIRA_CONF and SPIRA_TOML at nonexistent paths so no host config leaks verdicts
# into the suite (law-gates-run-in-a-clean-environment): lib.sh sources conf.sh into THIS
# process, and an ambient SPIRA_TOML — set for an operator's own shell convenience —
# would otherwise be read ahead of the fixture and inject a real persona roster.
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
export SPIRA_TOML="$T/no-such.toml"
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
# A quota-recording mock: the CPUQuota=70%/90% rows this fixture used to carry moved to
# test-summon-fayth.sh (D2/UC-dispatch-23). The express-grant test below still needs an
# argv-capturing mock, so it stays.
# ==========================================================================================
ARGS_FILE="$T/summon-args"
MOCK_QUOTA="$T/summon-quota"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\nexit 0\n' "$ARGS_FILE" > "$MOCK_QUOTA"
chmod +x "$MOCK_QUOTA"

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
# Restore the real chamber for the remaining, chamber-file-level sections
# ==========================================================================================
# conf.sh is idempotent (SPIRA_CONF_LOADED guards it), so re-sourcing lib.sh here does NOT
# recompute the SPIRA_*_LABEL defaults. The real chamber fayths reference several of them as
# BARE variables (builder.fayth's FAYTH_EXCLUDE_LABELS uses $SPIRA_ASK_LABEL/$SPIRA_CI_LABEL
# with no `:-` guard), so unsetting one here would make fayth_get's sourcing subshell die on
# an unbound variable under `set -u` and return empty for every field on every fayth — every
# persona then misreads as an operator persona. The block above pinned each of these to
# exactly conf.sh's own default, so they are left set rather than unset.
unset SPIRA_FAYTHS SPIRA_REPO_MAP SPIRA_DB
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
    schema.sh name no-such-partition-xyz 2>&1)" && schema_exit=0 || schema_exit=$?

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
        schema.sh name "$key" 2>/dev/null)" && key_exit=0 || key_exit=$?
    if [ "$key_exit" -eq 0 ] && [ -n "$label" ]; then
        ok "schema_name '$key' is declared, resolves to: $label"
    else
        bad "schema_name '$key' is NOT declared in schema.sh" \
            "exit=$key_exit, output='$label'"
    fi
done

# DISCRIMINATING: a custom SPIRA_PLAN_LABEL must propagate through schema_name.
custom_label="$(SPIRA_HOME="$HERE" SPIRA_CONF="$SCHEMA_T" \
    SPIRA_PLAN_LABEL=work schema.sh name plan 2>/dev/null)"
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
