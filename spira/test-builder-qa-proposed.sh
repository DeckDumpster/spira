#!/usr/bin/env bash
#
# test-builder-qa-proposed.sh — builder.fayth must exclude qa-proposed so that
#   a QA proposal cannot be claimed as approved work.
#
#   ./test-builder-qa-proposed.sh
#
# THE DEFECT. qa.md tells the QA aeon to file proposals labelled spira,plan,qa-proposed.
# plan IS builder.fayth's partition predicate, and qa-proposed was not in FAYTH_EXCLUDE_LABELS,
# so a proposal was indistinguishable from approved work the instant it was written. A builder
# claimed one; while working it filed more proposals; those summoned more builders. 247
# qa-proposed beads were filed in eight hours by ten builder aeons. (sp-2mhcs)
#
# THE FIX. Add qa-proposed to FAYTH_EXCLUDE_LABELS in builder.fayth.
#
# WHAT THIS SUITE CHECKS.
#   1. builder.fayth's FAYTH_EXCLUDE_LABELS contains qa-proposed (the direct field check).
#   2. When fayth_ready asks ready_count for the builder, qa-proposed appears in the
#      exclude argument (the end-to-end predicate check).
#
# WHAT THIS SUITE DOES NOT USE. No bd, no systemd, no network. ready_count is stubbed
# to capture its arguments without touching the database.
#
# tier: T1
# covers: spira/chamber/builder.fayth spira/lib.sh
# hermetic-ok: no database, no systemd; ready_count and SPIRA_SUMMON are stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

export SPIRA_HOME="$HERE"
export SPIRA_RUN="$T/run"
export SPIRA_CONF="$T/no-such.conf"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-builder-qa-proposed.sh"

# ==========================================================================================
echo
echo "builder.fayth — FAYTH_EXCLUDE_LABELS contains qa-proposed"
# ==========================================================================================
# A POSITIVE CONTROL AGAINST THE REAL FILE. This assertion fails if qa-proposed is removed
# from FAYTH_EXCLUDE_LABELS in builder.fayth, which is the field the predicate reads.
builder_excl="$(fayth_get builder FAYTH_EXCLUDE_LABELS)"
want "builder FAYTH_EXCLUDE_LABELS contains qa-proposed" "qa-proposed" "$builder_excl"

# Positive control: the include labels are still correct.
builder_labels="$(fayth_get builder FAYTH_LABELS)"
want "builder FAYTH_LABELS contains plan" "plan" "$builder_labels"

# ==========================================================================================
echo
echo "fayth_ready builder — qa-proposed appears in the exclude argument to ready_count"
# ==========================================================================================
# Override ready_count to capture the EXCLUDE argument (second positional) it receives.
# fayth_ready calls: ready_count "$FAYTH_LABELS" "$(fayth_exclude ...)" inside a subshell,
# so we write to a file rather than a variable.
EXCL_FILE="$T/observed-excl"
MOCK_READY=0
ready_count() {
    # $1 = include labels, $2 = exclude labels
    printf '%s' "$2" > "$EXCL_FILE"
    printf '%d' "$MOCK_READY"
}
# Stub aeon_count so fayth_free does not block the ready_count call.
aeon_count() { printf '0'; }

fayth_ready builder >/dev/null 2>&1 || true
observed_excl="$(cat "$EXCL_FILE" 2>/dev/null)"
want "fayth_ready builder passes qa-proposed in the exclude arg" "qa-proposed" "$observed_excl"

# ==========================================================================================
echo
echo "positive control — qa-proposed is NOT in ops.fayth exclusions (it must be deliberate)"
# ==========================================================================================
# Proves the label is not in the shared base — if it were, the builder fix would be trivially
# satisfied by something unrelated, and removing it from builder.fayth would not be detected.
ops_excl="$(fayth_get ops FAYTH_EXCLUDE_LABELS)"
lack "ops FAYTH_EXCLUDE_LABELS does not contain qa-proposed (distinct from builder)" \
     "qa-proposed" "$ops_excl"

echo
tl_summary
