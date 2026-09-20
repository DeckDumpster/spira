#!/usr/bin/env bash
#
# test-scope-label.sh — SPIRA_SCOPE_LABEL is a runtime key, not a literal.
#
# WHAT THIS SUITE VERIFIES
# 1. Default derives from SPIRA_HOME_REPO, not the literal "spira": a non-spira home repo
#    gets its own scope; a missing home repo gets an empty scope (never "spira").
# 2. Custom value: builder predicate becomes that value + plan; spira,plan is NOT the predicate.
# 3. Empty value: builder predicate is plan alone; no predicate contains an empty label
#    (a leading comma would match nothing, looking exactly like "no work ready").
# 4. fayth_fenced passes when the scope label is present, fails when it is absent,
#    and passes unconditionally when the scope label is empty.
#
# THE POSITIVE CONTROL (item 2) IS CRITICAL. A predicate that ignores the key entirely
# still passes "default=spira,plan" and "custom label is claimable". Only the second
# half of item 2 — "spira,plan is NOT the predicate under a custom key" — distinguishes
# a working implementation from a no-op.
#
# defect: law-scope-is-a-runtime-key (sp-9xsjm), sp-4mpy6
# covers: spira/conf.sh spira/lib.sh spira/chamber/*.fayth
# hermetic-ok: no database, no systemd; fayth_get and fayth_fenced are tested from lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL — %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# fayth_get_with_scope <scope_label> <fayth> <var>
# Evaluate one fayth variable under a specific SPIRA_SCOPE_LABEL, in a subprocess so
# the calling shell's exports are unaffected.
fayth_get_with_scope() {
    local scope="$1" fayth="$2" var="$3"
    SPIRA_HOME="$HERE" SPIRA_RUN="$T/run" SPIRA_CONF="$T/no-such.conf" \
    SPIRA_SCOPE_LABEL="$scope" \
        bash -c '. "$SPIRA_HOME/lib.sh" 2>/dev/null; fayth_get "$1" "$2"' \
             _ "$fayth" "$var" 2>/dev/null
}

# conf_scope_with_home <home_repo> — resolve SPIRA_SCOPE_LABEL from conf.sh defaults for a
# given SPIRA_HOME_REPO, without setting SPIRA_SCOPE_LABEL so the default path is exercised.
conf_scope_with_home() {
    local home_repo="$1"
    SPIRA_HOME="$HERE" SPIRA_RUN="$T/run" SPIRA_CONF="$T/no-such.conf" \
    SPIRA_HOME_REPO="$home_repo" \
        bash -c '. "$SPIRA_HOME/lib.sh" 2>/dev/null; printf "%s" "$SPIRA_SCOPE_LABEL"'
}

echo "test-scope-label.sh"

# ==========================================================================================
echo
echo "default — SPIRA_SCOPE_LABEL derives from SPIRA_HOME_REPO, not the literal spira"
# ==========================================================================================
# THE DISCRIMINATING TEST. A fixed literal "spira" passes when home repo IS spira; only the
# non-spira case separates a derived default from a hardcoded one.
got="$(conf_scope_with_home "myproject")"
is   "scope defaults to home repo name (myproject)" "myproject" "$got"
lack "scope default for non-spira install does NOT contain spira" "spira" "$got"

got="$(conf_scope_with_home "spira")"
is   "scope defaults to spira when home repo is spira" "spira" "$got"

# An install with no resolvable home repo must not silently produce the literal "spira".
got="$(conf_scope_with_home "")"
lack "scope with empty home repo does NOT produce spira" "spira" "$got"

# ==========================================================================================
echo
echo "default (SPIRA_SCOPE_LABEL=spira) — builder predicate is spira,plan"
# ==========================================================================================
got="$(fayth_get_with_scope "spira" builder FAYTH_LABELS)"
is "builder FAYTH_LABELS with scope=spira is spira,plan" "spira,plan" "$got"

# ==========================================================================================
echo
echo "custom scope (SPIRA_SCOPE_LABEL=other) — builder sees other,plan, NOT spira,plan"
# ==========================================================================================
# THE DISCRIMINATING TEST. A predicate that ignores SPIRA_SCOPE_LABEL entirely would produce
# "spira,plan" for both the default and this case. This assertion fails for that no-op.
got="$(fayth_get_with_scope "other" builder FAYTH_LABELS)"
is   "builder FAYTH_LABELS with scope=other is other,plan" "other,plan" "$got"
lack "builder FAYTH_LABELS with scope=other does NOT contain spira,plan" "spira,plan" "$got"

# Same check for ops (uses a different partition label).
got="$(fayth_get_with_scope "other" ops FAYTH_LABELS)"
is   "ops FAYTH_LABELS with scope=other is other,incident" "other,incident" "$got"

# ==========================================================================================
echo
echo "empty scope (SPIRA_SCOPE_LABEL=) — builder predicate is plan alone, no leading comma"
# ==========================================================================================
# A leading comma is an empty label and would match nothing, looking like "no work ready".
got="$(fayth_get_with_scope "" builder FAYTH_LABELS)"
is   "builder FAYTH_LABELS with scope= is just plan"   "plan" "$got"
lack "builder FAYTH_LABELS with scope= has no comma"   ","    "$got"
lack "builder FAYTH_LABELS with scope= has no spira"   "spira" "$got"

got="$(fayth_get_with_scope "" ops FAYTH_LABELS)"
is   "ops FAYTH_LABELS with scope= is just incident" "incident" "$got"

# ==========================================================================================
echo
echo "fayth_fenced — scope label present: pass; absent: fail; scope empty: pass"
# ==========================================================================================
# Source lib.sh once for the fayth_fenced calls below.
export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

# Scope=spira; label contains spira: pass.
SPIRA_SCOPE_LABEL=spira fayth_fenced test-persona "spira,plan" 2>/dev/null \
    && ok "fayth_fenced passes when scope label is present in predicate" \
    || bad "fayth_fenced passes when scope label is present in predicate" "returned non-zero"

# Scope=spira; label does NOT contain spira: fail.
SPIRA_SCOPE_LABEL=spira fayth_fenced test-persona "plan" 2>/dev/null \
    && bad "fayth_fenced fails when scope label is absent from predicate" "returned zero" \
    || ok  "fayth_fenced fails when scope label is absent from predicate"

# Scope=other; label contains other: pass.
SPIRA_SCOPE_LABEL=other fayth_fenced test-persona "other,plan" 2>/dev/null \
    && ok "fayth_fenced passes when custom scope label is present" \
    || bad "fayth_fenced passes when custom scope label is present" "returned non-zero"

# Scope=other; label contains spira but not other: fail.
SPIRA_SCOPE_LABEL=other fayth_fenced test-persona "spira,plan" 2>/dev/null \
    && bad "fayth_fenced fails when predicate has old scope label, not new one" "returned zero" \
    || ok  "fayth_fenced fails when predicate has old scope label, not new one"

# Scope= (empty): any non-empty predicate passes, no scope to enforce.
SPIRA_SCOPE_LABEL="" fayth_fenced test-persona "plan" 2>/dev/null \
    && ok "fayth_fenced passes when scope label is empty (unrestricted)" \
    || bad "fayth_fenced passes when scope label is empty (unrestricted)" "returned non-zero"

# Empty predicate still fails even when scope is empty.
SPIRA_SCOPE_LABEL="" fayth_fenced test-persona "" 2>/dev/null \
    && bad "fayth_fenced fails on empty FAYTH_LABELS even with empty scope" "returned zero" \
    || ok  "fayth_fenced fails on empty FAYTH_LABELS even with empty scope"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
