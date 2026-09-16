#!/usr/bin/env bash
#
# test-fayth-predicates.sh — fayth predicates select on schema-validated fields, not bare
# partition literals.
#
# WHAT THIS SUITE VERIFIES
# 1. No active .fayth file contains a bare partition literal in FAYTH_LABELS — asserted by
#    scanning the source, with a positive control that proves the scanner fires before we
#    rely on its silence (law-absence-needs-a-positive-control).
# 2. Every active persona resolves to a non-empty partition label at runtime (fayth_get
#    actually expands $SPIRA_PLAN_LABEL etc., rather than handing an unexpanded string to bd).
# 3. REGRESSION (sp-xrkuu class): schema_name must fail closed on an undeclared key, not
#    return 0. A reader that asks for a partition it cannot name returned [] from bd, which
#    the refusal guard read as "no work" — printing a confident 0 against a true 32.
#
# The positive control for item 3: plan and incident are declared names, so schema_name
# succeeds on them. If either were absent here, the fayth-level expansion below would still
# resolve (the default is baked into the variable form), but a reader that calls
# schema_name plan would error — catching a misconfiguration rather than silently using the
# wrong label.
#
# defect: sp-xrkuu (partition query returned [] due to unexpanded shell in the label)
# covers: spira/chamber/*.fayth spira/schema.sh spira/conf.sh

# covers: spira/chamber/*.fayth spira/schema.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT

# A FAYTH_LABELS line with a bare partition literal ends with }word" — where the character
# immediately after } is a lowercase letter (variable references end with }$VARNAME").
# This regex identifies the defective form.
BARE_PATTERN='}[a-z][a-z_-]*"'

# ==========================================================================================
echo "POSITIVE CONTROL: bare-literal scanner finds an offender"
# ==========================================================================================
# Plant an offender and prove the grep fires on it. Without this, a grep pointed at the
# wrong file or with a pattern that never matches is indistinguishable from a clean tree
# (law-absence-needs-a-positive-control).

mkdir -p "$T/chamber"
printf 'FAYTH_NAME=offender\nFAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}plan"\n' \
    > "$T/chamber/offender.fayth"

if grep -qE "FAYTH_LABELS=.*$BARE_PATTERN" "$T/chamber/offender.fayth" 2>/dev/null; then
    ok "positive control: bare-literal scanner fires on a planted offender"
else
    bad "positive control: bare-literal scanner DID NOT fire on a planted offender" \
        "the check is broken — cannot trust its silence"
fi

# ==========================================================================================
echo ""
echo "NO BARE PARTITION LITERALS in active chamber fayths"
# ==========================================================================================
# Each persona's FAYTH_LABELS must resolve through a variable ($SPIRA_*_LABEL), never be
# written as a literal. A literal cannot be changed at runtime, cannot be validated by
# schema.sh at write time, and cannot be distinguished from a successful empty query when bd
# returns []. The form }$VAR_NAME" is the required one; }literal" is the defective one.

any_bare=0
for f in "$HERE/chamber/"*.fayth; do
    [ -e "$f" ] || continue
    name="${f##*/}"; name="${name%.fayth}"
    src="$(cat "$f")"
    # Check non-comment lines only (a comment about a literal is not a literal).
    noncomment="$(grep -v '^[[:space:]]*#' "$f" 2>/dev/null)"
    if printf '%s\n' "$noncomment" | grep -qE "FAYTH_LABELS=.*$BARE_PATTERN"; then
        bad "$name.fayth: FAYTH_LABELS contains a bare partition literal" \
            "$(printf '%s\n' "$noncomment" | grep 'FAYTH_LABELS=')"
        any_bare=1
    else
        ok "$name.fayth: FAYTH_LABELS resolves through a variable, not a bare literal"
    fi
done

# ==========================================================================================
echo ""
echo "SPECIFIC FAYTHS: builder uses \$SPIRA_PLAN_LABEL, ops uses \$SPIRA_INCIDENT_LABEL"
# ==========================================================================================
# Assert the exact correct form (not just the absence of the bad one), per the pattern in
# test-spike.sh: the predicate is built from the configured label, not a hardcoded one.

builder_src="$(cat "$HERE/chamber/builder.fayth" 2>/dev/null)"
want   "builder FAYTH_LABELS is built from \$SPIRA_PLAN_LABEL" \
       'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_PLAN_LABEL"' \
       "$builder_src"
nowant "builder FAYTH_LABELS does NOT hardcode plan" \
       '}plan"' \
       "$(grep -v '^[[:space:]]*#' "$HERE/chamber/builder.fayth" 2>/dev/null)"

ops_src="$(cat "$HERE/chamber/ops.fayth" 2>/dev/null)"
want   "ops FAYTH_LABELS is built from \$SPIRA_INCIDENT_LABEL" \
       'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_INCIDENT_LABEL"' \
       "$ops_src"
nowant "ops FAYTH_LABELS does NOT hardcode incident" \
       '}incident"' \
       "$(grep -v '^[[:space:]]*#' "$HERE/chamber/ops.fayth" 2>/dev/null)"

# ==========================================================================================
echo ""
echo "EVERY ACTIVE PERSONA resolves to a non-empty partition at runtime"
# ==========================================================================================
# fayth_get sources the fayth in a subshell with conf.sh in scope, so $SPIRA_PLAN_LABEL
# expands to its configured value. An unset or empty label would produce an empty FAYTH_LABELS,
# which the cockpit renders as ? (correct) but the sentinel would see as a malformed predicate.
#
# "ACTIVE" MEANS SUMMONABLE, AND AN OPERATOR PERSONA IS NOT. A predicate carves a partition of
# the graph for something that CLAIMS beads; the concierge claims none, runs for days as the
# operator's own session, and a partition would be actively wrong — a bead it held is a bead no
# aeon could work. Its empty FAYTH_LABELS is a declaration, which is why `fayth_partitions` and
# the cockpit's partition map both skip a fayth without one.
#
# The exclusion is keyed on FAYTH_SUMMON rather than on the name, because a check that
# recognised "concierge" by string would bind the wrong persona the day it is renamed or a
# second operator role is added beside it.

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"; mkdir -p "$T/run"
export SPIRA_CONF="$T/no.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh" 2>/dev/null

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

# AND THE EXCLUSION MUST NOT HAVE EMPTIED THE LOOP. Skipping every persona would satisfy every
# assertion above by having made none of them (law-absence-needs-a-positive-control).
if [ "$_checked" -gt 0 ]; then
    ok "$_checked summonable persona(s) were actually checked"
else
    bad "the predicate loop checked nothing" "every fayth was skipped as operator-summoned"
fi

# ==========================================================================================
echo ""
echo "REGRESSION (sp-xrkuu class): schema_name fails closed on undeclared keys"
# ==========================================================================================
# The defect class: a reader that asks for a partition by name gets back the empty string
# (or an unexpanded variable) and hands it to bd as a label query. bd answers [] truthfully
# and the caller reads "no work" instead of "error". schema_name must exit non-zero on any
# key it does not know, so the error is unmistakable.

# POSITIVE CONTROL: schema_name fails on a made-up key before we check the real ones.
schema_out="$(SPIRA_HOME="$HERE" SPIRA_CONF="$T/no.conf" \
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
    label="$(SPIRA_HOME="$HERE" SPIRA_CONF="$T/no.conf" \
        bash "$HERE/schema.sh" name "$key" 2>/dev/null)" && key_exit=0 || key_exit=$?
    if [ "$key_exit" -eq 0 ] && [ -n "$label" ]; then
        ok "schema_name '$key' is declared, resolves to: $label"
    else
        bad "schema_name '$key' is NOT declared in schema.sh" \
            "exit=$key_exit, output='$label'"
    fi
done

# DISCRIMINATING: a custom SPIRA_PLAN_LABEL must propagate through schema_name.
# If schema_name returned a hardcoded default rather than reading the variable, this would
# pass even on broken code — so pin to a non-default to distinguish them.
custom_label="$(SPIRA_HOME="$HERE" SPIRA_CONF="$T/no.conf" \
    SPIRA_PLAN_LABEL=work bash "$HERE/schema.sh" name plan 2>/dev/null)"
if [ "$custom_label" = "work" ]; then
    ok "schema_name plan: SPIRA_PLAN_LABEL=work propagates to 'work', not 'plan'"
else
    bad "schema_name plan: SPIRA_PLAN_LABEL=work did not propagate" \
        "expected 'work', got '$custom_label'"
fi

# ==========================================================================================
echo ""
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
