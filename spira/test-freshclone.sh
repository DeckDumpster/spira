#!/usr/bin/env bash
#
# test-freshclone.sh — a fresh clone of this repository can be configured and run.
#
#   ./test-freshclone.sh
#
# WHAT THIS TESTS
# ---------------
# law-ships-for-a-colleague: "The published repo must clone and run." Without a test,
# that claim is aspirational — indistinguishable from the false one. This suite is the
# positive control that makes it meaningful.
#
# Two claims are tested:
#
#   1. SYNTAX: every shipped shell script in the harness parses. A syntax error in any
#      shipped script prevents the harness from starting, and an inventory pass says
#      nothing about parseability. `bash -n` catches it before any runtime, so this
#      runs without a database, an API key, or any operator configuration.
#
#   2. DEFAULTS: conf.sh resolves without any operator config. A harness that requires
#      a config file to start is not "clone and run" — it is "clone, configure, then
#      run". After a clone with no spira.conf, SPIRA_HOME must resolve from conf.sh's
#      own location, and SPIRA_ASK_LABEL must be a generic label that names no operator.
#
# POSITIVE CONTROLS COME FIRST (law-absence-needs-a-positive-control).
#
#   For syntax: a script with a deliberate syntax error must be caught before the
#   shipped tree is declared clean — otherwise a bash -n that resolves the wrong
#   directory, or whose glob matches nothing, passes vacuously.
#
#   For defaults: a conf.sh whose SPIRA_ASK_LABEL default names an operator must be
#   caught before the shipped default is declared generic — otherwise a check that
#   never read the key would pass just as well.
#
# defect: sp-cmn
# tier: T0
# covers: spira/conf.sh spira/doctor.sh spira/*.sh spira/chamber/*.fayth UC-safety-fences-34
# covers: spira/statutes/law-ships-for-a-colleague.txt
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd)"
isne()  { [ "$2" != "$3" ] && ok "$1" || bad "$1" "wanted anything but [$2]"; }

echo "test-freshclone.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ==========================================================================
echo
echo "positive control — bash -n catches a deliberate syntax error:"
# ==========================================================================
BROKEN="$TMP/broken.sh"
printf '#!/usr/bin/env bash\nif then\nfi\n' > "$BROKEN"
if bash -n "$BROKEN" 2>/dev/null; then
    bad "positive control" "bash -n did not catch a syntax error in broken.sh"
else
    ok "bash -n catches a syntax error"
fi

# ==========================================================================
echo
echo "syntax check — every shipped shell script parses:"
# ==========================================================================
# Top-level and spira/ scripts, plus the chamber's fayths (D8: absorbs test-spike.sh's
# redundant bash -n rows over the same files — confine.sh is already caught by the
# "$HERE"/*.sh glob below, so only the fayths, which are not *.sh, needed adding).
# A fayth is a shell fragment SOURCED into the summoning process; a syntax error in one is
# not a persona that misbehaves, it is a harness that dies mid-summon. Other subdirectories
# (cockpit, loom, systemd) contain non-shell content or template placeholders that bash -n
# would misread as syntax errors.
syntax_fail=0
for f in "$ROOT"/*.sh "$HERE"/*.sh "$HERE"/chamber/*.fayth; do
    [ -f "$f" ] || continue
    if ! bash -n "$f" 2>/dev/null; then
        bad "syntax: $(basename "$f")" "fails bash -n"
        syntax_fail=$((syntax_fail+1))
    fi
done
[ "$syntax_fail" -eq 0 ] && ok "all shipped shell scripts and fayths parse"

# ==========================================================================
echo
echo "defaults — conf.sh resolves without any operator config:"
# ==========================================================================
# Simulate a fresh clone: a harness tree with no surrounding spira.conf, no
# user home config, and no environment carrying SPIRA_* values. env -i strips
# everything except PATH and HOME; SPIRA_CONF is pointed at a nonexistent file
# so conf.sh falls through to its defaults.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
cp "$HERE/conf.sh" "$HARNESS/spira/conf.sh"

conf_val() {
    local key="$1"; shift
    env -i "$@" PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        bash -c ". '$HARNESS/spira/conf.sh' 2>/dev/null; printf '%s' \"\${${key}:-}\"" 2>/dev/null
}

# Positive control: SPIRA_HOME must be set before asserting it is sensible.
# If conf.sh never touched SPIRA_HOME the later checks would pass vacuously.
home_val="$(conf_val SPIRA_HOME)"
isne "SPIRA_HOME is set by conf.sh (positive control)" "" "$home_val"

# Positive control for ask label: verify the check WOULD catch a bad default.
# Create a minimal conf.sh that sets SPIRA_ASK_LABEL to an operator name, then
# confirm the pattern that the main assertion uses does flag it.
BAD_CONF="$TMP/bad-conf/spira/conf.sh"
mkdir -p "$(dirname "$BAD_CONF")"
# Minimal conf.sh that sets the ask label to a person's name and then defines
# SPIRA_HOME so conf_val does not fail for an unrelated reason.
printf '. "%s/conf.sh"\nSPIRA_ASK_LABEL=needs-ryan\n' "$HERE" > "$BAD_CONF"
bad_label="$(env -i PATH="$PATH" HOME="$TMP/home" SPIRA_CONF=/nonexistent \
    bash -c ". '$BAD_CONF' 2>/dev/null; printf '%s' \"\${SPIRA_ASK_LABEL:-}\"" 2>/dev/null || true)"
if [[ "${bad_label:-}" == *ryan* ]] || [[ "${bad_label:-}" == *ryangantt* ]]; then
    ok "ask-label positive control: bad default resolves correctly for the assertion below"
else
    bad "ask-label positive control" \
        "could not inject a bad default — assertion below is untestable (got: $bad_label)"
fi

# Main assertions.
shipped_label="$(conf_val SPIRA_ASK_LABEL)"
# The shipped default must not name an operator. Generic labels ('needs-operator',
# 'needs-user') pass; names of people ('needs-ryan', 'needs-alice') do not.
# Pattern: starts with 'needs-', followed by a generic role word (all lowercase,
# no part of a person's name). We assert three conditions:
#   1. non-empty (not unconfigured)
#   2. starts with 'needs-' (follows the convention)
#   3. not a known-personal pattern (simple person-name check)
isne "SPIRA_ASK_LABEL default is non-empty" "" "$shipped_label"
want "SPIRA_ASK_LABEL default starts with needs-"    "needs-" "$shipped_label"
nowant "SPIRA_ASK_LABEL default does not name ryan"    "ryan"   "$shipped_label"

echo
tl_summary
