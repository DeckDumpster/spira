#!/usr/bin/env bash
#
# test-rune.sh — the persona roster and predicates: discovery, own-partition selection,
#   rune_exclude, and the fields the shipped runes must carry.
#
#   ./test-rune.sh
#
# T1 for dispatch UC-07/08. Merges test-rune-predicates.sh's runtime rows (the exact
# source-text rows are deleted: the runtime override rows below already prove the property
# that a predicate is built from a variable, not a literal), test-lanes.sh's roster-split
# and real-roster rows, and test-spike.sh's rune-fields row.
#
# UC-dispatch-13 (effective lanes: .spira/modes ∩ repo-map lanes ∩ SPIRA_RUNES, all of the
# former test-effective-lanes.sh) was retired at sp-27hsi along with the dead functions it
# drove (_spira_lane_diag, _spira_modes_lanes, _spira_runes_lane_set): nothing calls that
# three-way-intersection-with-named-refusal diagnostic any more. Lane admission in the live
# path is mimir-trigger/groom-trigger.sh's much simpler spira_lane_admitted (any repo-map
# lanes column membership, no .spira/modes or SPIRA_RUNES intersection, no refusal naming),
# which stays and is exercised through mimir-trigger's own mocked-port unit tests, not a
# bash suite. See docs/test-plan/dispatch.toml for the UC-dispatch-13 [use_case.uncovered].
#
# THE DEFECT (sp-rune-predicate). sentinel.sh CHECK 7 computed one $ready from the
# builder's own predicate and gated every persona's call on it, so ops never woke on its
# own work. call_rune now asks rune_ready, which sources the rune file and asks
# ready_count through THAT rune's RUNE_LABELS. spira_runes enumerates runes/*.rune
# rather than returning a literal name, so installing a rune is the whole registration.
#
# WHAT THIS SUITE DOES NOT USE. No bd, no systemd, no network for the library-level rows;
# ready_count and SPIRA_CALL are stubs. Section-local env overrides are restored (and
# lib.sh re-sourced) before the next section relies on the real runes again.
#
# defect: sp-rune-predicate sp-czsf4 sp-xrkuu
# tier: T1
# covers: spira/lib.sh sentinel/src/* spira/schema.sh spira/conf.sh doctor/src/* spira/runes/*.rune spira-claim/* UC-dispatch-07 UC-dispatch-08
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# Run with the real runes so the suite exercises the installed runes, not a model of
# them. SPIRA_CONF and SPIRA_TOML at nonexistent paths so no host config leaks verdicts
# into the suite (law-gates-run-in-a-clean-environment): lib.sh sources conf.sh into THIS
# process, and an ambient SPIRA_TOML — set for an operator's own shell convenience —
# would otherwise be read ahead of the fixture and inject a real persona roster.
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$T/no-such.conf"
# Registered config (SPIRA_RUN) is declared via tl_config into testlib.sh's own override
# layer rather than pinning a private SPIRA_TOML: that layered SPIRA_TOML is already the
# clean, fully-specified environment law-gates-run-in-a-clean-environment asks for — no
# ambient operator config can reach it either way.
# SPIRA_RUNES_DIR too: the complete fixture declares a non-empty value, and nothing derives
# it from SPIRA_HOME any more (sfail round 2, pattern 6) — without this the real runes
# (builder.rune, ops.rune, ...) is never found.
tl_config SPIRA_RUN="$T/run" SPIRA_RUNES_DIR="$HERE/runes"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# rune_ready (wave 4.25, sp-obhv6) is a one-line shim onto `spira-claim rune-ready`, and
# its ready set is the lifecycle machine's READY rows (sp-v62vn: there is no off mode, so no
# `bd ready` call whose --label argv this suite could read). The fixture is a STORE instead:
# `set_store <json>` sets the beads a fake `$SPIRA_BD` serves to every `list`, a spira-lc
# stand-in (testlib lc_mirror_bd) makes each open one a READY row, and spira-claim's own
# predicate decides which persona's ready set it lands in — so each row below asserts what
# a persona's predicate TAKES, not the query string it built.
STORE_FILE="$T/store.json"
FAKE_BD="$T/fake-bd"
cat > "$FAKE_BD" <<EOF
#!/bin/sh
case " \$* " in *" list "*) cat "$STORE_FILE" 2>/dev/null || echo '[]' ;; *) echo '[]' ;; esac
EOF
chmod +x "$FAKE_BD"
export SPIRA_DB="/fake/db"
# Declared both ways: spira-claim resolves SPIRA_BD via cfg() (tl_config), but the
# spira-lc stand-in lc_mirror_bd installs is a plain bash stub reading ${SPIRA_BD:-bd}
# straight from its own inherited env, never through config (sfail round 3, pattern 3/7).
export SPIRA_BD="$FAKE_BD"
tl_config SPIRA_BD="$FAKE_BD"
lc_mirror_bd "$T/lc"
export PATH="$T/lc:$PATH"
# bead_json <id> <comma-labels> — one open task bead carrying exactly those labels.
bead_json() {
    local l; l="$(printf '%s' "$2" | python3 -c 'import json,sys; print(json.dumps([x for x in sys.stdin.read().split(",") if x]))')"
    printf '{"id":"%s","title":"fixture","status":"open","issue_type":"task","priority":1,"labels":%s}' "$1" "$l"
}
set_store() { printf '[%s]\n' "$1" > "$STORE_FILE"; }
set_store ""

# Stub dwarf_count so no running dwarves are reported: rune_free would otherwise see a
# live slot and return 0, blocking the call path before ready_count is called.
dwarf_count() { printf '0'; }

# Stub SPIRA_CALL so call_rune does not attempt to start a systemd unit.
# It records each call, so a row can tell "called" from "nothing ready".
MOCK_CALL="$T/call"
CALLED="$T/called"
printf '#!/bin/sh\necho called >> "%s"\nexit 0\n' "$CALLED" > "$MOCK_CALL"
chmod +x "$MOCK_CALL"
calls() { if [ -f "$CALLED" ]; then grep -c . "$CALLED"; else echo 0; fi; }   # no file: none yet
export SPIRA_CALL="$MOCK_CALL"

# ==========================================================================================
# spira_runes — discovers every rune in the runes, not just one
# ==========================================================================================
unset SPIRA_RUNES 2>/dev/null || true
got="$(spira_runes)"
# The real runes has builder and ops at minimum; both must appear.
want "builder appears in roster" "builder" "$got"
want "ops appears in roster"     "ops"     "$got"

# ==========================================================================================
# ops.rune — its RUNE_LABELS is its OWN partition, not the builder's
# ==========================================================================================
# A POSITIVE CONTROL AGAINST THE REAL FILE. The defect was that ops.rune carried a correct
# predicate that the harness never used; this assertion fails if the file is ever edited to
# carry the builder's predicate, or if rune_get resolves the wrong file.
ops_labels="$(rune_get ops RUNE_LABELS)"
nowant "ops RUNE_LABELS is not the builder's spira,plan" "spira,plan" " $ops_labels "
want "ops RUNE_LABELS contains its own partition"     "incident"   "$ops_labels"

# ==========================================================================================
# rune_ready / call_rune — ops uses ITS OWN predicate, not the builder's
# ==========================================================================================
builder_labels="$(rune_get builder RUNE_LABELS)"
INCIDENT_BEAD="$(bead_json sp-fy-inc "$ops_labels")"
PLAN_BEAD="$(bead_json sp-fy-plan "$builder_labels")"

# POSITIVE CONTROL: ops's own incident bead is in ops's ready set, and calls ops.
set_store "$INCIDENT_BEAD"; rm -f "$CALLED"
is "ops's ready set holds its own incident bead" "1" "$(rune_ready ops 2>/dev/null)"
call_rune ops >/dev/null 2>&1 || true
is "ops is called for its own incident bead" "1" "$(calls)"

set_store "$PLAN_BEAD"; rm -f "$CALLED"
is "ops does NOT take the builder's plan bead" "0" "$(rune_ready ops 2>/dev/null)"
sum_out="$(call_rune ops 2>&1)" || true
[ "$(calls)" = 0 ] && ok "and ops is not called for it" \
    || bad "and ops is not called for it" "calls=$(calls); call_rune said: $sum_out"

# ==========================================================================================
# rune_ready — builder uses ITS OWN predicate
# ==========================================================================================
is "builder's ready set holds its own plan bead" "1" "$(rune_ready builder 2>/dev/null)"
set_store "$INCIDENT_BEAD"
is "builder does NOT take ops's incident bead" "0" "$(rune_ready builder 2>/dev/null)"

# ==========================================================================================
# positive control — old single-predicate approach misses ops work
# ==========================================================================================
# THE OLD CODE IN THREE LINES:
#   ready="$(ready_count spira,plan ...)"   # hardcoded builder predicate
#   if [ "$ready" -gt 0 ]; then             # gates ALL personas, including ops
#       for f in $RUNES; do call_rune "$f"; done
#   fi
# Over a store holding only ops's incident bead, the builder's predicate reads 0 — and ops
# must still be called on its own predicate.
set_store "$INCIDENT_BEAD"; rm -f "$CALLED"
plan_ready="$(ready_count "$builder_labels" "")"
is "old code: plan_ready=0 (no plan work)" "0" "$plan_ready"
call_rune ops >/dev/null 2>&1 || true
is "ops is called on its own predicate even when plan_ready=0" "1" "$(calls)"

# ==========================================================================================
# A quota-recording mock: the CPUQuota=70%/90% rows this fixture used to carry moved to
# test-call-rune.sh (D2/UC-dispatch-23). The express-grant test below still needs an
# argv-capturing mock, so it stays.
# ==========================================================================================
ARGS_FILE="$T/call-args"
MOCK_QUOTA="$T/call-quota"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\nexit 0\n' "$ARGS_FILE" > "$MOCK_QUOTA"
chmod +x "$MOCK_QUOTA"

# ==========================================================================================
# call_rune — an express grant restricts the claim to express beads
# ==========================================================================================
# call_rune's third argument is carried to the dwarf as SPIRA_REQUIRE_EXPRESS, so the dwarf
# started under an express grant cannot claim a non-express bead.
export SPIRA_CALL="$MOCK_QUOTA"

set_store "$PLAN_BEAD"; rm -f "$ARGS_FILE"
call_rune builder 1 >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
nowant "no express grant: SPIRA_REQUIRE_EXPRESS is absent from args" "SPIRA_REQUIRE_EXPRESS" "$args"

# The express grant counts only beads whose lifecycle row is express (call.rs: ready-count
# --express), so this store's plan bead is on the mirror's express list.
set_store "$(bead_json sp-fy-express "$builder_labels")"; rm -f "$ARGS_FILE"
echo sp-fy-express > "$T/lc/express"
call_rune builder 1 express >/dev/null 2>&1 || true
args="$(cat "$ARGS_FILE" 2>/dev/null)"
want "express grant: SPIRA_REQUIRE_EXPRESS=1 appears in args" \
     "SPIRA_REQUIRE_EXPRESS=1" "$args"

export SPIRA_CALL="$MOCK_CALL"

# ==========================================================================================
# D5 — spira_lane_runes / spira_task_runes: the roster split (from test-lanes.sh)
# ==========================================================================================
# A synthetic runes, neither rune named "builder" or "ops" — the rule must be bound to
# the RUNE_LANE declaration, not to a hardcoded name.
LANES_HOME="$T/lanes-home"
mkdir -p "$LANES_HOME/runes"
# SPIRA_HOME IS THE HOME NOW (locate_home no longer searches): every binary reads
# <home>/conf.d directly, so a synthetic runes with none refuses config resolution
# outright (sfail round 2, pattern 1).
ln -s "$HERE/conf.d" "$LANES_HOME/conf.d"
cat > "$LANES_HOME/runes/worker.rune" <<'F'
RUNE_NAME=worker
RUNE_LABELS="spira,plan"
RUNE_MAX_CONCURRENT=2
RUNE_ELASTIC=1
F
cat > "$LANES_HOME/runes/warden.rune" <<'F'
RUNE_NAME=warden
RUNE_LABELS="spira,incident"
RUNE_MAX_CONCURRENT=1
RUNE_LANE=priority
F

export SPIRA_HOME="$LANES_HOME"
# SPIRA_RUNES via tl_config too: cmd_rune's roster is "the declared one (spira.runes),
# never an environment override" (spira-config/src/main.rs, cmd_rune) — the lib.sh bash
# wrapper's raw-env forwarding to the spira-config subprocess is a leftover from before
# the one-source migration and is now ignored for this exact-match roster (round 5).
tl_config SPIRA_RUNES="worker warden" SPIRA_RUNES_DIR="$LANES_HOME/runes"

lane="$(spira_lane_runes)"
task="$(spira_task_runes)"

is "spira_lane_runes returns the RUNE_LANE rune"      "warden" "$lane"
is "spira_task_runes excludes the RUNE_LANE rune"     "worker"   "$task"
nowant "the lane rune does not appear in task runes"   "warden" "$task"
nowant "the task rune does not appear in lane runes"   "worker"   "$lane"

# tl_config's SPIRA_RUNES persists in the override file (no per-call scoping) until
# something changes it again — every later section in this file that relies on the
# fixture's own default roster (builder/ops/groomer etc.) via a plain `export SPIRA_RUNES`
# would otherwise keep seeing "worker warden" here (round 6).
spira-config unset spira.runes "$_TL_CONF_OVERRIDE" >/dev/null

# POSITIVE CONTROL: both functions return something, so absence above is the exclusion
# working and not both functions returning empty.
is "there is at least one lane rune"  "1" "$([ -n "$lane" ] && echo 1 || echo 0)"
is "there is at least one task rune"  "1" "$([ -n "$task" ] && echo 1 || echo 0)"

# ==========================================================================================
# D5 — the real roster: ops is a lane rune, builder is a task rune (from test-lanes.sh)
# ==========================================================================================
export SPIRA_HOME="$HERE"
export SPIRA_RUNES="builder ops"
tl_config SPIRA_RUNES_DIR="$HERE/runes"

real_task="$(spira_task_runes)"
real_lane="$(spira_lane_runes)"

want   "ops appears in lane runes" "ops" "$real_lane"
nowant "ops does NOT appear in task runes" "ops" "$real_task"
want   "builder is still in the task pool" "builder" "$real_task"

unset SPIRA_RUNES

# ==========================================================================================
# T0 — no active rune carries a bare partition literal (from test-rune-predicates.sh)
# ==========================================================================================
# A RUNE_LABELS line with a bare partition literal ends with }word" — where the character
# immediately after } is a lowercase letter (variable references end with }$VARNAME"). The
# exact-string checks that used to sit here (builder built from $SPIRA_PLAN_LABEL, ops from
# $SPIRA_INCIDENT_LABEL, and their negations) are deleted: the runtime override rows above
# already prove each persona asks through its own configured label, which is the property
# those source-text checks existed to protect.
BARE_PATTERN='}[a-z][a-z_-]*"'

# POSITIVE CONTROL: plant an offender and prove the grep fires on it first
# (law-absence-needs-a-positive-control).
mkdir -p "$T/bare-scan"
printf 'RUNE_NAME=offender\nRUNE_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan"\n' \
    > "$T/bare-scan/offender.rune"
if grep -qE "RUNE_LABELS=.*$BARE_PATTERN" "$T/bare-scan/offender.rune" 2>/dev/null; then
    ok "positive control: bare-literal scanner fires on a planted offender"
else
    bad "positive control: bare-literal scanner DID NOT fire on a planted offender" \
        "the check is broken — cannot trust its silence"
fi

for f in "$HERE/runes/"*.rune; do
    [ -e "$f" ] || continue
    name="${f##*/}"; name="${name%.rune}"
    # Comment lines are excluded — a comment about a literal is not a literal.
    noncomment="$(grep -v '^[[:space:]]*#' "$f" 2>/dev/null)"
    if printf '%s\n' "$noncomment" | grep -qE "RUNE_LABELS=.*$BARE_PATTERN"; then
        bad "$name.rune: RUNE_LABELS contains a bare partition literal" \
            "$(printf '%s\n' "$noncomment" | grep 'RUNE_LABELS=')"
    else
        ok "$name.rune: RUNE_LABELS resolves through a variable, not a bare literal"
    fi
done

# ==========================================================================================
# Restore the real runes for the remaining, runes-file-level sections
# ==========================================================================================
# conf.sh is idempotent (SPIRA_CONF_LOADED guards it), so re-sourcing lib.sh here does NOT
# recompute the SPIRA_*_LABEL defaults. The real runes runes reference several of them as
# BARE variables (builder.rune's RUNE_EXCLUDE_LABELS uses $SPIRA_ASK_LABEL/$SPIRA_CI_LABEL
# with no `:-` guard), so unsetting one here would make rune_get's sourcing subshell die on
# an unbound variable under `set -u` and return empty for every field on every rune — every
# persona then misreads as an operator persona. The block above pinned each of these to
# exactly conf.sh's own default, so they are left set rather than unset.
unset SPIRA_RUNES SPIRA_REPO_MAP SPIRA_DB
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$T/no-such.conf"
tl_config SPIRA_RUN="$T/run" SPIRA_RUNES_DIR="$HERE/runes"
# shellcheck disable=SC1090
. "$HERE/lib.sh" 2>/dev/null

# ==========================================================================================
# UC-dispatch-08 — every callable persona resolves to a non-empty predicate at runtime
# (from test-rune-predicates.sh)
# ==========================================================================================
# rune_get sources the rune in a subshell with conf.sh in scope, so $SPIRA_PLAN_LABEL
# expands to its configured value. An unset or empty label would produce an empty
# RUNE_LABELS, which the cockpit renders as ? (correct) but the sentinel would see as a
# malformed predicate.
#
# "CALLABLE" EXCLUDES AN OPERATOR PERSONA: the concierge claims nothing and runs for days
# as the operator's own session, so a partition would be actively wrong. The exclusion is
# keyed on RUNE_CALL rather than on the name, because a check that recognised
# "concierge" by string would bind the wrong persona the day it is renamed.
_checked=0
for f in $(rune_names 2>/dev/null); do
    if [ "$(rune_get "$f" RUNE_CALL auto 2>/dev/null)" != auto ]; then
        ok "$f: operator persona, no predicate expected"
        continue
    fi
    _checked=$(( _checked + 1 ))
    labels="$(rune_get "$f" RUNE_LABELS '' 2>/dev/null)"
    if [ -n "$labels" ]; then
        ok "$f: RUNE_LABELS resolves to non-empty: $labels"
    else
        bad "$f: RUNE_LABELS resolved to empty" "predicate is missing"
    fi
done

# AND THE EXCLUSION MUST NOT HAVE EMPTIED THE LOOP (law-absence-needs-a-positive-control).
if [ "$_checked" -gt 0 ]; then
    ok "$_checked callable persona(s) were actually checked"
else
    bad "the predicate loop checked nothing" "every rune was skipped as operator-called"
fi

# ==========================================================================================
# REGRESSION (sp-xrkuu class) — schema_name fails closed on undeclared keys
# (from test-rune-predicates.sh)
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

# plan and incident must be declared — a partition the rune uses must be one schema_name
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
tl_config SPIRA_PLAN_LABEL=work
custom_label="$(SPIRA_HOME="$HERE" SPIRA_CONF="$SCHEMA_T" \
    schema.sh name plan 2>/dev/null)"
is "schema_name plan: SPIRA_PLAN_LABEL=work propagates to 'work', not 'plan'" \
   "work" "$custom_label"

# ==========================================================================================
# D4 — the spike rune's own fields (from test-spike.sh)
# ==========================================================================================
# The predicate is built from the configured label rather than a literal, which is the
# property that keeps the rune, the brief and confine.sh agreeing.
rune_src="$(cat "$HERE/runes/spike.rune")"
want "spike: the predicate is built from the configured label" \
     'RUNE_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_SPIKE_LABEL"' "$rune_src"
nowant "spike: and does not hardcode one" 'RUNE_LABELS="spira,spike"' "$rune_src"
want "spike: a spike may search the web" "WebSearch" "$rune_src"
want "spike: and it may build"           "Bash"      "$rune_src"
want "spike: and edit"                   "Edit"      "$rune_src"

# ==========================================================================================
# G16 — rune_exclude adds every OTHER persona's rune:<name> label and the queue-wait label
# ==========================================================================================
# lib.sh l.988: today rune_exclude is exercised only indirectly, through the detectors that
# call rune_ready/call_rune. A direct row proves the exclusion set itself: every OTHER
# persona's rune:<name>, plus $SPIRA_QUEUE_WAIT_LABEL, never the caller's own name.
export SPIRA_RUNES="builder ops groomer"
tl_config SPIRA_QUEUE_WAIT_LABEL="g16-queue-wait"

excl="$(rune_exclude ops "ops-own-label")"
want   "G16: the persona's own exclude labels are kept"          "ops-own-label"  "$excl"
want   "G16: SPIRA_QUEUE_WAIT_LABEL is added"                     "g16-queue-wait" "$excl"
want   "G16: excludes another persona's rune: label (builder)"  "rune:builder"  "$excl"
want   "G16: excludes another persona's rune: label (groomer)"  "rune:groomer"  "$excl"
nowant "G16: does not exclude its own rune: label (ops)"        "rune:ops"      "$excl"

spira-config unset spira.queue_wait_label "$_TL_CONF_OVERRIDE" >/dev/null
unset SPIRA_QUEUE_WAIT_LABEL SPIRA_RUNES

# ==========================================================================================
# G17 — the roster survives conf.sh's own environment: SPIRA_HOME and SPIRA_RUNES set but NOT
# exported, as production has them (sp-nki5w). The rune shims exec spira-config; a bare exec
# saw neither and production's roster came back empty, while this suite — which exports
# SPIRA_HOME — stayed green.
# ==========================================================================================
roster="$(export -n SPIRA_HOME; SPIRA_RUNES="builder ops"; export -n SPIRA_RUNES; spira_runes 2>&1)"
want   "G17: roster with SPIRA_HOME unexported names builder"     "builder"  "$roster"
want   "G17: roster honours an unexported SPIRA_RUNES (ops)"     "ops"      "$roster"
nowant "G17: no SPIRA_HOME refusal under conf.sh's environment"   "SPIRA_HOME is not set" "$roster"
names="$(export -n SPIRA_HOME; rune_names 2>&1)"
want   "G17: rune_names with SPIRA_HOME unexported lists builder" "builder"  "$names"

tl_summary
