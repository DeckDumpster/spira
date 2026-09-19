#!/usr/bin/env bash
#
# test-incident-cause.sh — every SPIRA_INCIDENT_REF filing site also exports
# SPIRA_INCIDENT_CAUSE (law-producers-declare-what-they-know).
#
# THE PROPERTY. A producer that sets SPIRA_INCIDENT_REF without SPIRA_INCIDENT_CAUSE
# files recurrences into the undifferentiated "unrecorded" bucket, collapsing the
# census taxonomy and preventing Maechen's ranking step from telling causes apart.
# This check ensures each filing site carries both variables.
#
# THE ACCEPTANCE CRITERION. The grep finds every SPIRA_INCIDENT_REF= assignment in
# spira/*.sh (excluding test suites), then checks whether any of the surrounding 14
# lines (10 before, 3 after) contain SPIRA_INCIDENT_CAUSE. A site with no cause is
# UNDECLARED. After this fix the set must be empty.
#
# THE POSITIVE CONTROL (law-absence-needs-a-positive-control). Before asserting that
# the real spira/ tree is clean, this suite plants one offending site in a temporary
# script and requires the checker to name it. A mis-scoped glob, an off-by-one window,
# or a grep that finds nothing all look like "clean" without the control.
#
# defect: sp-crov
# covers: spira/*.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }

echo "test-incident-cause.sh"

# check_undeclared <dir> — emit one "UNDECLARED <file>:<line>" per SPIRA_INCIDENT_REF
# site that lacks a SPIRA_INCIDENT_CAUSE in its surrounding 14 lines.
check_undeclared() {
    local dir="$1"
    grep -rn "SPIRA_INCIDENT_REF=" --include='*.sh' "$dir" | grep -v '/test-' | \
    while IFS=: read -r f l r; do
        [ "$(sed -n "$((l-10)),$((l+3))p" "$f" | grep -c SPIRA_INCIDENT_CAUSE)" -eq 0 ] \
            && printf 'UNDECLARED %s:%s\n' "$f" "$l"
    done
}

# --- positive control: the checker must find a planted offender -----------------
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
OFFENDER="$TMP/offender.sh"
# A minimal env block with SPIRA_INCIDENT_REF but no SPIRA_INCIDENT_CAUSE — exactly
# the shape every real site had before this fix.
cat > "$OFFENDER" <<'SCRIPT'
SPIRA_DB="$SPIRA_DB" \
SPIRA_INCIDENT_TYPE=task \
SPIRA_INCIDENT_ACTOR=test \
SPIRA_INCIDENT_REPO=spira \
SPIRA_INCIDENT_REF=incident:planted-offender \
bash incident.sh file "planted test" -
SCRIPT

control_out="$(check_undeclared "$TMP" 2>/dev/null)"
if printf '%s\n' "$control_out" | grep -q "UNDECLARED.*offender.sh"; then
    ok "positive control: checker finds the planted offender"
else
    bad "positive control" "checker did not find offender.sh; out=[${control_out:-<empty>}]"
fi

# --- real property: no undeclared sites in spira/ --------------------------------
real_out="$(check_undeclared "$HERE" 2>/dev/null)"
if [ -z "$real_out" ]; then
    ok "spira/ has no SPIRA_INCIDENT_REF sites without SPIRA_INCIDENT_CAUSE"
else
    bad "undeclared sites in spira/" "$(printf '%s\n' "$real_out")"
fi

# --- summary ---------------------------------------------------------------------
printf '\n%s: %d passed, %d failed\n' "test-incident-cause.sh" "$pass" "$fail"
[ "$fail" -eq 0 ]
