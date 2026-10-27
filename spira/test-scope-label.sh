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
#
# THE POSITIVE CONTROL (item 2) IS CRITICAL. A predicate that ignores the key entirely
# still passes "default=spira,plan" and "custom label is claimable". Only the second
# half of item 2 — "spira,plan is NOT the predicate under a custom key" — distinguishes
# a working implementation from a no-op.
#
# RETIRED (sp-j89pd, wave 4.2): rune_fenced (lib.sh) had zero live callers — it is now
# aeon::conf::rune_fenced (aeon/src/conf.rs) and release::stage (release/src/stage.rs).
# Its "scope label present/absent/empty" table, item 4 of this suite, is deleted with it.
#
# defect: law-scope-is-a-runtime-key (sp-9xsjm), sp-4mpy6
# tier: T1
# covers: spira/conf.sh spira/lib.sh spira/runes/*.rune
# hermetic-ok: no database, no systemd; rune_get is tested from lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

# rune_get_with_scope <scope_label> <rune> <var>
# Evaluate one rune variable under a specific SPIRA_SCOPE_LABEL, in a subprocess so
# the calling shell's exports are unaffected. SPIRA_RUN/SPIRA_SCOPE_LABEL are registered
# keys (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): declare via tl_config, not the env
# prefix below, which no process reads them from any more.
rune_get_with_scope() {
    local scope="$1" rune="$2" var="$3"
    # round 3 fix (pattern 6): SPIRA_RUNES_DIR no longer derives from SPIRA_HOME — without
    # it, rune_get cannot find runes/builder.rune or runes/ops.rune at all.
    tl_config SPIRA_RUN="$T/run" SPIRA_SCOPE_LABEL="$scope" SPIRA_RUNES_DIR="$HERE/runes"
    SPIRA_HOME="$HERE" SPIRA_CONF="$T/no-such.conf" \
        bash -c '. "$SPIRA_HOME/lib.sh" 2>/dev/null; rune_get "$1" "$2"' \
             _ "$rune" "$var" 2>/dev/null
}

# conf_scope_with_home() and the "default — SPIRA_SCOPE_LABEL derives from SPIRA_HOME_REPO"
# section it drove are REMOVED (per Ryan 2026-10-05, ONE SOURCE OF CONFIG): they proved
# conf.sh's derived-default path when SPIRA_SCOPE_LABEL was absent from both env and
# config, a case that no longer exists now that the checked-in complete fixture declares
# scope_label on every suite's SPIRA_TOML base layer, with no way to make it absent.

echo "test-scope-label.sh"

# ==========================================================================================
echo
echo "default (SPIRA_SCOPE_LABEL=spira) — builder predicate is spira,plan"
# ==========================================================================================
got="$(rune_get_with_scope "spira" builder RUNE_LABELS)"
is "builder RUNE_LABELS with scope=spira is spira,plan" "spira,plan" "$got"

# ==========================================================================================
echo
echo "custom scope (SPIRA_SCOPE_LABEL=other) — builder sees other,plan, NOT spira,plan"
# ==========================================================================================
# THE DISCRIMINATING TEST. A predicate that ignores SPIRA_SCOPE_LABEL entirely would produce
# "spira,plan" for both the default and this case. This assertion fails for that no-op.
got="$(rune_get_with_scope "other" builder RUNE_LABELS)"
is   "builder RUNE_LABELS with scope=other is other,plan" "other,plan" "$got"
lack "builder RUNE_LABELS with scope=other does NOT contain spira,plan" "spira,plan" "$got"

# Same check for ops (uses a different partition label).
got="$(rune_get_with_scope "other" ops RUNE_LABELS)"
is   "ops RUNE_LABELS with scope=other is other,incident" "other,incident" "$got"

# ==========================================================================================
echo
echo "empty scope (SPIRA_SCOPE_LABEL=) — builder predicate is plan alone, no leading comma"
# ==========================================================================================
# A leading comma is an empty label and would match nothing, looking like "no work ready".
got="$(rune_get_with_scope "" builder RUNE_LABELS)"
is   "builder RUNE_LABELS with scope= is just plan"   "plan" "$got"
lack "builder RUNE_LABELS with scope= has no comma"   ","    "$got"
lack "builder RUNE_LABELS with scope= has no spira"   "spira" "$got"

got="$(rune_get_with_scope "" ops RUNE_LABELS)"
is   "ops RUNE_LABELS with scope= is just incident" "incident" "$got"

echo
tl_summary
