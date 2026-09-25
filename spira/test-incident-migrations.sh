#!/usr/bin/env bash
#
# test-incident-migrations.sh — incident.sh's one-time migration subcommands
# (gap G9: backfill-ref-labels; UC-ops-detection-remediation-07b: backfill-recur-causes).
#
#   ./test-incident-migrations.sh
#
# NEITHER MIGRATION IS TESTED TODAY (gap G9). backfill-ref-labels is what the label-keyed
# dedup path (UC-04) depends on for beads filed before the ref: label existed — an untested
# migration that silently mislabels or skips a bead leaves that bead on the slow O(N)
# fallback path forever, or worse, unfindable. backfill-recur-causes is ported here from
# test-incident-recur-cause.sh, which deleted its own copy (UC-07b) — retiring the migration
# itself is proposed separately, not decided in this suite.
#
# A STUB BD (incident-stub-bd.py), not a fixture database: both migrations are pure
# list-then-label-then-relist sequences over beads this suite plants itself, and neither
# reads the events table or anything else a stub cannot answer.
#
# tier: T1
# covers: spira/incident.sh spira/incident-stub-bd.py
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-migrations.sh"

INC="$HERE/incident.sh"
STUB_BD="$HERE/incident-stub-bd.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"
export STUB_BD_STATE="$TMP/state.json" STUB_BD_LOG="$TMP/bd.log"

run_migration() {  # run_migration <subcommand>
    env -i HOME="$HOME" PATH="$PATH" \
        SPIRA_BD="$STUB_BD" STUB_BD_STATE="$STUB_BD_STATE" STUB_BD_LOG="$STUB_BD_LOG" \
        SPIRA_DB="fakedb" SPIRA_RUN="$TMP/run" SPIRA_CONF="$TMP/no-conf" \
        SPIRA_INCIDENT_LOCK="$TMP/run/migrations-test.lock" \
        bash "$INC" "$1" 2>&1
}
seed() {  # seed <json-bead-on-stdin>
    env STUB_BD_STATE="$STUB_BD_STATE" python3 "$STUB_BD" seed
}
labels_of() {
    python3 -c '
import json
d = json.load(open("'"$STUB_BD_STATE"'"))
print(" ".join(d["beads"].get("'"$1"'", {}).get("labels", [])))
'
}
has_label() { [[ " $(labels_of "$1") " == *" $2 "* ]]; }

# ======================================================================================
echo
echo "1. backfill-ref-labels — an open bead with external_ref and no ref: label is labelled (gap G9):"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
printf '{"id":"sp-old1","external_ref":"incident:legacy-one","status":"open","labels":["spira","partition:incident"]}' | seed
printf '{"id":"sp-old2","external_ref":"incident:legacy-two","status":"open","labels":["spira","partition:incident"]}' | seed

out="$(run_migration backfill-ref-labels)"
want "reports how many were backfilled" "backfilled 2" "$out"

_expect_hash="$(python3 -c 'import hashlib; print(hashlib.sha256(b"incident:legacy-one").hexdigest()[:8])')"
has_label sp-old1 "ref:$_expect_hash" \
    && ok "the legacy bead gets ref:<hash-of-external-ref>" \
    || bad "the legacy bead gets ref:<hash-of-external-ref>" "labels: $(labels_of sp-old1)"

# ======================================================================================
echo
echo "2. backfill-ref-labels is idempotent — a bead already carrying a ref: label is skipped:"
# ======================================================================================
out2="$(run_migration backfill-ref-labels)"
want "second run reports 0 backfilled" "backfilled 0" "$out2"

# ======================================================================================
echo
echo "3. backfill-ref-labels skips a bead with no external_ref:"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
printf '{"id":"sp-noref","external_ref":"","status":"open","labels":["spira","partition:incident"]}' | seed
out3="$(run_migration backfill-ref-labels)"
want "a bead with no external_ref is not counted" "backfilled 0" "$out3"
is "and it gets no ref: label" "" "$(labels_of sp-noref | grep -o 'ref:[0-9a-f]*' || true)"

# ======================================================================================
echo
echo "4. backfill-recur-causes converts bare sp-recur-N to sp-recur-N-unrecorded (UC-07b, ported):"
# ======================================================================================
rm -f "$STUB_BD_STATE" "$STUB_BD_LOG"
printf '{"id":"sp-bare","external_ref":"incident:bare-recur","status":"open","labels":["spira","partition:incident","sp-recur-1","sp-recur-2"]}' | seed
printf '{"id":"sp-typed","external_ref":"incident:typed-recur","status":"open","labels":["spira","partition:incident","sp-recur-1-suite-red"]}' | seed

out4="$(run_migration backfill-recur-causes)"
want "backfill reports 1 bead backfilled" "backfilled 1" "$out4"
want "backfill reports 0 errors"          "errors 0"     "$out4"

has_label sp-bare "sp-recur-1-unrecorded" \
    && ok "bare sp-recur-1 promoted to sp-recur-1-unrecorded" \
    || bad "bare sp-recur-1 promoted to sp-recur-1-unrecorded" "labels: $(labels_of sp-bare)"
has_label sp-bare "sp-recur-2-unrecorded" \
    && ok "bare sp-recur-2 promoted to sp-recur-2-unrecorded" \
    || bad "bare sp-recur-2 promoted to sp-recur-2-unrecorded" "labels: $(labels_of sp-bare)"

# Bare labels must be removed, not just joined by typed ones (double-counting).
_bare_gone="$(labels_of sp-bare | tr ' ' '\n' | grep -xE 'sp-recur-1|sp-recur-2' || true)"
[ -z "$_bare_gone" ] && ok "bare sp-recur-N labels removed after backfill" \
    || bad "bare sp-recur-N labels removed after backfill" "still present: $_bare_gone"

# The already-typed bead is the idempotence positive control: untouched.
has_label sp-typed "sp-recur-1-suite-red" \
    && ok "an already-typed bead is untouched by backfill" \
    || bad "an already-typed bead is untouched by backfill" "labels: $(labels_of sp-typed)"

# ======================================================================================
echo
echo "5. backfill-recur-causes is idempotent (safe to re-run):"
# ======================================================================================
out5="$(run_migration backfill-recur-causes)"
want "second run reports 0 backfilled" "backfilled 0" "$out5"

echo
tl_summary
