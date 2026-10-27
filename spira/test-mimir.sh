#!/usr/bin/env bash
#
# test-mimir.sh — mimir.fayth and chamber/mimir.md: the retrospective persona
#   and its brief encode the five-step pass, the three-occurrence threshold, the
#   four-property output bar, and the per-pass output bound.
#
#   ./test-mimir.sh
#
# WHAT THIS SUITE GUARDS
# ----------------------
# mimir.fayth must declare a lane so it draws from its own capacity rather than the
# task pool, and must not set an assignee.
#
# mimir.md must explicitly encode all four items the bead requires. The test checks
# by grep so it fails if a future edit removes any of the required properties.
#
# conf.sh must declare SPIRA_MIMIR_* keys with correct defaults, and the mimir
# lane must appear in SPIRA_LANES.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control)
# -------------------------------------------------------
# Each brief property is verified by grepping for a string that would NOT appear in a
# brief that omits the property. A brief with no positive-control discussion passes all
# four of the other checks; the "positive control" grep is the one that fails.
#
# tier: T0
# covers: spira/chamber/mimir.fayth spira/chamber/mimir.md spira/conf.sh UC-ops-detection-remediation-17
# hermetic-ok: no database, no systemd; reads files only
# scar: unrecorded
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

lack() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in output" ;; *) ok "$1" ;; esac; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/no-such.conf"
FAYTH="$HERE/chamber/mimir.fayth"
BRIEF="$HERE/chamber/mimir.md"

# SPIRA_CONFIG_DIR: the directory holding spira-config on THIS invocation's own PATH, so
# the stripped-down PATH built for run_conf below can still find it — conf.sh now refuses
# outright without it (sp-ubcgo: "wave 4.5: conf.sh becomes an eval of resolve"). Resolved
# once, from testenv's/the gate's own PATH (where the staged release's spira-config lives),
# never guessed.
SPIRA_CONFIG_DIR="$(command -v spira-config >/dev/null 2>&1 && dirname "$(command -v spira-config)" || true)"

run_conf() {
    # env -i clears SPIRA_TOML; conf.sh refuses outright without it (per Ryan 2026-10-05,
    # ONE SOURCE OF CONFIG), so thread testlib's own hermetic fixture through.
    env -i HOME="$T" PATH="${SPIRA_CONFIG_DIR:+$SPIRA_CONFIG_DIR:}/usr/bin:/bin" SPIRA_CONF="$NONE" \
        SPIRA_TOML="$SPIRA_TOML" \
        bash -c ". '$HERE/conf.sh' && $1" 2>/dev/null
}

echo "test-mimir.sh"

# ==========================================================================================
echo
echo "mimir.fayth — file exists"
# ==========================================================================================
if [ -f "$FAYTH" ]; then ok "mimir.fayth exists"; else bad "mimir.fayth" "not found at $FAYTH"; fi

# ==========================================================================================
echo
echo "mimir.md — file exists"
# ==========================================================================================
if [ -f "$BRIEF" ]; then ok "mimir.md exists"; else bad "mimir.md" "not found at $BRIEF"; fi

# ==========================================================================================
echo
echo "mimir.fayth — FAYTH_LANE=mimir (draws from own capacity, not the task pool)"
# ==========================================================================================
lane="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_LANE:-}\"")"
is "FAYTH_LANE=mimir" "mimir" "$lane"

# ==========================================================================================
echo
echo "mimir.fayth — no FAYTH_ASSIGNEE set (partition labels come from predicate only)"
# ==========================================================================================
assignee="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_ASSIGNEE:-UNSET}\"")"
is "FAYTH_ASSIGNEE is not set" "UNSET" "$assignee"

# ==========================================================================================
echo
echo "mimir.fayth — FAYTH_EXCLUDE_LABELS names no hold label (poison/ask are lifecycle holds, sp-psztcc)"
# ==========================================================================================
excl="$(run_conf ". '$FAYTH' && printf '%s' \"\${FAYTH_EXCLUDE_LABELS:-}\"")"
nowant "FAYTH_EXCLUDE_LABELS names no spira-poison" "spira-poison" "$excl"

# ==========================================================================================
echo
echo "spira_fayths — mimir appears in the persona roster"
# ==========================================================================================
# POSITIVE CONTROL: enumerating chamber/*.fayth discovers mimir automatically; this fails
# if the file is removed or renamed.
export SPIRA_HOME="$HERE"
export SPIRA_CONF="$NONE"
export SPIRA_RUN="$T"
# round 3 fix (pattern 6): SPIRA_CHAMBER no longer derives from SPIRA_HOME — without it,
# spira_fayths cannot enumerate chamber/*.fayth at all.
tl_config SPIRA_RUN="$SPIRA_RUN" SPIRA_CHAMBER="$HERE/chamber"
. "$HERE/lib.sh"
roster="$(spira_fayths)"
want "mimir appears in spira_fayths" "mimir" "$roster"
want "builder also appears (roster is not empty)"  "builder"  "$roster"

# ==========================================================================================
echo
echo "conf.sh — SPIRA_MIMIR_LABEL is in the key list and defaults to mimir-sweep"
# ==========================================================================================
keys="$(run_conf 'printf "%s" "$SPIRA_CONF_KEYS"')"
want "SPIRA_MIMIR_LABEL is in the key list"           "SPIRA_MIMIR_LABEL"           "$keys"
want "SPIRA_MIMIR_REMEDY_LABEL is in the key list"    "SPIRA_MIMIR_REMEDY_LABEL"    "$keys"
want "SPIRA_MIMIR_LANDING_INTERVAL is in the key list" "SPIRA_MIMIR_LANDING_INTERVAL" "$keys"
want "SPIRA_MIMIR_MAX_GAP_SECONDS is in the key list" "SPIRA_MIMIR_MAX_GAP_SECONDS" "$keys"
want "SPIRA_MIMIR_MAX_BEADS is in the key list"       "SPIRA_MIMIR_MAX_BEADS"       "$keys"

lbl="$(run_conf 'printf "%s" "$SPIRA_MIMIR_LABEL"')"
is "SPIRA_MIMIR_LABEL defaults to mimir-sweep" "mimir-sweep" "$lbl"

remedy="$(run_conf 'printf "%s" "$SPIRA_MIMIR_REMEDY_LABEL"')"
is "SPIRA_MIMIR_REMEDY_LABEL defaults to mimir-remedy" "mimir-remedy" "$remedy"

max_beads="$(run_conf 'printf "%s" "$SPIRA_MIMIR_MAX_BEADS"')"
is "SPIRA_MIMIR_MAX_BEADS defaults to 3" "3" "$max_beads"

gap="$(run_conf 'printf "%s" "$SPIRA_MIMIR_MAX_GAP_SECONDS"')"
is "SPIRA_MIMIR_MAX_GAP_SECONDS defaults to 10800" "10800" "$gap"

interval="$(run_conf 'printf "%s" "$SPIRA_MIMIR_LANDING_INTERVAL"')"
is "SPIRA_MIMIR_LANDING_INTERVAL defaults to 25" "25" "$interval"

# ==========================================================================================
echo
echo "conf.sh — mimir lane is in SPIRA_LANES default"
# ==========================================================================================
lanes="$(run_conf 'printf "%s" "$SPIRA_LANES"')"
want "mimir is in SPIRA_LANES" "mimir" "$lanes"

# ==========================================================================================
echo
echo "mimir.md — encodes the five-step pass"
# ==========================================================================================
brief="$(cat "$BRIEF")"
want "brief contains Census step"   "Census"   "$brief"
want "brief contains Select step"   "Select"   "$brief"
want "brief contains Diagnose step" "Diagnose" "$brief"
want "brief contains Design step"   "Design"   "$brief"
want "brief contains Record step"   "Record"   "$brief"

# ==========================================================================================
echo
echo "mimir.md — encodes the three-causal-event threshold (sp-jcd0e)"
# ==========================================================================================
want "brief names the three-causal-event threshold" "three or more distinct causal events" "$brief"
want "brief explains two is a coincidence"        "coincidence"                "$brief"

# ==========================================================================================
echo
echo "mimir.md — encodes all four output-bar properties"
# ==========================================================================================
# Property 1: concrete location
want "brief names concrete location requirement" "file:line" "$brief"

# Property 2: observed evidence (command + output)
want "brief requires observed evidence (command output)" "command run and what it printed" "$brief"

# Property 3: acceptance criteria with positive control
want "brief requires acceptance criteria"  "Acceptance criteria"  "$brief"
want "brief requires a positive control"   "positive control"     "$brief"

# Property 4: failure class and occurrence count
want "brief requires failure class label"     "failure class"      "$brief"
want "brief requires occurrence count"        "occurrence count"   "$brief"

# ==========================================================================================
echo
echo "mimir.md — encodes the per-pass output bound"
# ==========================================================================================
want "brief references {{MAX_BEADS}} placeholder" "{{MAX_BEADS}}" "$brief"
want "brief explains flooding consequence"        "flooded"        "$brief"

# ==========================================================================================
echo
echo "mimir.md — no unresolved shell variable references"
# ==========================================================================================
# POSITIVE CONTROL: grep must catch a planted shell variable reference.
# Asserted in an env where those names are unset — if the brief still expands them,
# a rendered Mimir prompt will contain unresolved shell variables.
BRIEF_WITH_OFFENDER="$(printf '%s\n$SPIRA_FAKE_VAR extra line' "$brief")"
if grep -qE '\$(SPIRA_[A-Z_]+|BEAD_ID)' <<< "$BRIEF_WITH_OFFENDER"; then
    ok "no-shell-var check: positive control fires on a planted reference"
else
    bad "no-shell-var check: positive control failed — grep missed the planted variable"
fi
unresolved="$(grep -nE '\$(SPIRA_[A-Z_]+|BEAD_ID)' "$BRIEF" 2>/dev/null)"
if [ -z "$unresolved" ]; then
    ok "mimir.md has no unresolved shell variable references"
else
    bad "mimir.md has unresolved shell variable references" "$unresolved"
fi

# ==========================================================================================
echo
echo "mimir.md — encodes the closing rule (quiet pass is distinguishable)"
# ==========================================================================================
want "brief requires recording the pass" "mimir.log"      "$brief"
want "brief outlaws silence"             "Silence is what"  "$brief"

# ==========================================================================================
echo
echo "mimir.md — watermark advance is conditional on census exit code (sp-mimir-watermark-advances-blind)"
# ==========================================================================================
# The fix for sp-mimir-watermark-advances-blind: Step 5 must not advance the watermark
# unconditionally. The brief must capture census_rc in Step 1 and condition the advance on
# it in Step 5. A pass that follows these instructions literally cannot advance blind.
#
# POSITIVE CONTROL: each grep would fail if the brief were reverted to the unconditional
# form. "census_rc" does not appear in a brief that always advances; "HELD" does not appear
# in one that only logs "Watermark advanced to".
want "brief captures census_rc"                              "census_rc"          "$brief"
want "brief conditions advance on census success"            "only when the census succeeded" "$brief"
want "brief has HELD log format for failed census"           "Watermark HELD at"  "$brief"
want "brief has Watermark advanced log format for success"   "Watermark advanced to" "$brief"
want "brief says leave watermark when census fails"          "leave the watermark where it is" "$brief"

# ==========================================================================================
echo
echo "mimir.md — all {{SPIRA_HOME}} paths resolve to existing files"
# ==========================================================================================
# POSITIVE CONTROL: plant a doubled segment (e.g. {{SPIRA_HOME}}/spira/census.sh) and this
# fails. law-a-rename-repoints-no-reader: a persona edit that renames or moves a helper is
# caught by the same check without manual update.
#
# ZERO MATCHES IS NOW THE EXPECTED STATE, NOT AN ABSENT CONTROL. mimir.md's one
# {{SPIRA_HOME}}-prefixed reference (census.sh) was retired to a bare-name PATH lookup —
# `census --with-suppressed`, the same binary-call idiom skew/doctor use elsewhere — when
# census moved to Rust (sp-yyk47). A brief with no bash helpers left to reference by path
# has nothing for this check to verify; asserting "at least one {{SPIRA_HOME}} path" would
# demand this brief keep an unnecessary path prefix just to satisfy the test. If a future
# edit reintroduces a {{SPIRA_HOME}}/<path> reference, the loop above still verifies it
# resolves — that half of the check is unconditional and still real.
_path_count=0
while IFS= read -r rel; do
    _path_count=$((_path_count+1))
    if [ -f "$HERE/$rel" ]; then
        ok "persona path resolves: {{SPIRA_HOME}}/$rel"
    else
        bad "persona path missing" "{{SPIRA_HOME}}/$rel not found under $HERE"
    fi
done < <(grep -oE '\{\{SPIRA_HOME\}\}/[^"'"'"' )]+' "$BRIEF" | sed 's|^{{SPIRA_HOME}}/||' | sort -u)
if [ "$_path_count" -gt 0 ]; then
    ok "persona names at least one {{SPIRA_HOME}} path (positive-control anchor)"
else
    ok "persona names no {{SPIRA_HOME}} paths — census.sh's own reference retired to a bare binary lookup (sp-yyk47)"
fi

echo
tl_summary
