#!/usr/bin/env bash
# test-reopen-queue-eject.sh — the ejected-suites sidecar survives a failed re-certification
# (sp-px6ng): gate.sh keeps naming the ejecting suites after the eject. The sidecar lives in
# $SPIRA_RUN/ejected/<id>; the landing ledger it once sat beside is deleted (sp-2c1n0).
#
# WRITER, NOT A HAND COPY OF ITS RULE. Earlier versions of this suite wrote the sidecar
# with a bare printf and read it back with shell mirroring gate.sh's own if/elif — proving
# only that the test agreed with itself. This drives the real writer (the lib.sh calls
# `queue eject --red --suites` makes) and the real reader (gate.sh's own subprocess, via gate-fixture.sh),
# so a change to either side that breaks the contract shows up here.
#
# This stays in gate-verdict rather than moving to landing-merge-queue (as
# docs/test-plan/gate-verdict.md section 3 first proposed): gate.sh, this area's own
# subject script, is the reader, and landing-merge-queue has no test-plan file to receive
# it. The selection half that used to share this file (ejected suite added/CSV/dedup/
# absent-skipped) has moved to the suite-select crate's unit tests (gate::tests, UC-gate-verdict-09,
# sp-wx2tw).
#
# tier: T1
# covers: queue/src/* spira/lib.sh spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"
. "$HERE/testdb.sh"
command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }
testdb_require test-reopen-queue-eject

TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
gate_fixture_init "$TMP"
BR=spira/sp-ej1
mkdir -p "$REPO/spira"; printf '#!/bin/bash\n' > "$REPO/spira/test-other.sh"
git -C "$REPO" add -A; git -C "$REPO" commit -q -m "base: the suite the sidecar names"
git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
gate_fixture_branch "$BR"

# HOST MAILBOX SENTINEL (sp-rya9d): models whatever SPIRA_MAIL an outer process (an aeon
# session, or a batch container's own install) has already exported by the time this suite
# runs. conf.sh's `: "${SPIRA_MAIL:=...}"` only derives from SPIRA_RUN when SPIRA_MAIL is
# still unset — overriding SPIRA_RUN below is not enough once something upstream already set
# it, and that is exactly how a fixture ejection mail ("sp-ej1 ejected from PR 1") reached a
# real Concierge Maildir during a corpus run. The writer subshell must pin SPIRA_MAIL itself;
# this sentinel is the proof — its count must never move.
HOSTMAIL="$TMP/hostmail"
export SPIRA_MAIL="$HOSTMAIL"

_maildir_count() {  # _maildir_count <mailbox-dir> -> files in new/ + cur/
    local n=0 f
    for f in "$1"/new/* "$1"/cur/*; do
        [ -f "$f" ] && n=$((n + 1))
    done
    printf '%d' "$n"
}

# sp-ej1 exists in a real store so the writer section's bead_reopen is a real reopen
# against a real bead, not a refusal that happens to leave the sidecar behind.
testdb_up reopen-queue-eject || { echo "test-reopen-queue-eject: could not build fixture database"; exit 1; }
printf '{"id":"sp-ej1","title":"t","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}\n' \
    | testdb_seed

# A gate command that only ever reports what it was handed, so every case below is one
# gate.sh run rather than a hand-rolled read of the sidecar.
EJDUMP="$TMP/ejected-seen"
printf 'repo | %s | push | origin/main |  | echo EJECTED=$SPIRA_GATE_EJECTED_SUITES > %s; exit 0\n' \
    "$REPO" "$EJDUMP" > "$MAP"

seen_ejected() {   # seen_ejected <bead> -> the suites gate.sh's own subprocess was handed
    : > "$EJDUMP"
    gate_fixture_run "$BR" repo SPIRA_GATE_BEAD="$1" >/dev/null 2>&1
    sed -n 's/^EJECTED=//p' "$EJDUMP"
}

echo "test-reopen-queue-eject.sh"
echo

# ---------------------------------------------------------------------------
# POSITIVE CONTROL: a bead with no eject history reaches the gate command with nothing
# ejected. Without this, a suite below finding "test-other.sh" would prove nothing —
# it might always find it.
# ---------------------------------------------------------------------------
is "no eject history: nothing reaches the gate command" "" "$(seen_ejected sp-noeject)"

# ---------------------------------------------------------------------------
# THE REAL WRITER. verdict.sh's _attr_eject is retired (queue/DESIGN-verdict.md D1); a
# queue eject of a member that broke a test (`queue eject <id> --red --suites <csv>`) is
# the writer now: the eject itself is a lifecycle Returned event, and bead_reopen with the
# suites writes $SPIRA_RUN/ejected/<id>. SPIRA_DB points at the throwaway testdb fixture
# (sp-ej1 seeded open, so the reopen is a harmless no-op); SPIRA_RUN is the fixture's own
# RUN, so its ejected/ is the directory gate.sh's subprocess below reads.
# ---------------------------------------------------------------------------
_hostmail_before="$(_maildir_count "$HOSTMAIL/concierge")"
tl_config SPIRA_RUN="$RUN" SPIRA_MAIL="$RUN/mail" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-bd}"
(
    export HOME="$HOMEDIR" SPIRA_CONF="$SPIRA_CONF_NONE"
    . "$HERE/lib.sh"
    bead_reopen sp-ej1 eject-red "" test-other.sh >/dev/null 2>&1
)
is "writer: the sidecar names the ejecting suite" \
    "test-other.sh" "$(cat "$RUN/ejected/sp-ej1" 2>/dev/null)"
is "writer: no landing ledger is (re)created (sp-2c1n0)" \
    "absent" "$([ -e "$RUN/landstate" ] && echo present || echo absent)"
is "reader: gate.sh's own subprocess is handed the ejected suite" \
    "test-other.sh" "$(seen_ejected sp-ej1)"
is "host concierge Maildir is untouched by the fixture's eject" \
    "$_hostmail_before" "$(_maildir_count "$HOSTMAIL/concierge")"

# ---------------------------------------------------------------------------
# THE DEFECT (sp-px6ng): a failed re-certification used to overwrite the record that was the
# ejected suites' only home. The gate run above was that re-certification; the sidecar must
# outlive it and keep forcing the suite on the next one.
# ---------------------------------------------------------------------------
is "reader: the ejected suite still reaches gate.sh's subprocess after RED" \
    "test-other.sh" "$(seen_ejected sp-ej1)"

echo
tl_summary
