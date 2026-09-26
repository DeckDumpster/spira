#!/usr/bin/env bash
# test-reopen-queue-eject.sh — the .ejected sidecar survives a failed re-certification
# (sp-px6ng): gate.sh keeps naming the ejecting suites even after a RED overwrites the
# EJECTED landstate record that used to be their only home.
#
# WRITER, NOT A HAND COPY OF ITS RULE. Earlier versions of this suite wrote the sidecar
# with a bare printf and read it back with shell mirroring gate.sh's own if/elif — proving
# only that the test agreed with itself. This drives the real writer (verdict.sh's
# _attr_eject) and the real reader (gate.sh's own subprocess, via gate-fixture.sh),
# so a change to either side that breaks the contract shows up here.
#
# This stays in gate-verdict rather than moving to landing-merge-queue (as
# docs/test-plan/gate-verdict.md section 3 first proposed): gate.sh, this area's own
# subject script, is the reader, and landing-merge-queue has no test-plan file to receive
# it. The selection half that used to share this file (ejected suite added/CSV/dedup/
# absent-skipped) has moved to test-gate-touched.sh, which already owns UC-gate-verdict-09.
#
# tier: T1
# covers: spira/verdict.sh spira/gate.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
. "$HERE/testlib/gate-fixture.sh"
command -v flock >/dev/null 2>&1 || { echo "  SKIP  flock is not on PATH"; exit 77; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
gate_fixture_init "$TMP"
BR=spira/sp-ej1
gate_fixture_branch "$BR"

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
# THE REAL WRITER. verdict.sh's _attr_eject is what the merge queue calls when it ejects a
# branch; it reopens the bead, marks the landstate EJECTED, and writes the suites CSV to
# $LANDSTATE/<id>.ejected. SPIRA_DB points nowhere and SPIRA_RUN is the fixture's own RUN
# (so LANDSTATE here is the same directory gate.sh's subprocess below reads), so the bead
# and mail side effects _attr_eject also has fail closed and harmless rather than reaching
# anything real (law-run-in-explicit-minimal-environment).
# ---------------------------------------------------------------------------
(
    export HOME="$HOMEDIR" SPIRA_CONF="$SPIRA_CONF_NONE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB_NONE"
    . "$HERE/verdict.sh"
    _attr_eject sp-ej1 deadbeef test-other.sh 1 repo suite-overlap "" "" "" "" "" "" >/dev/null 2>&1
)
is "writer: landstate reads EJECTED after _attr_eject" \
    "EJECTED" "$(cut -d' ' -f1 < "$RUN/landstate/sp-ej1" 2>/dev/null)"
is "reader: gate.sh's own subprocess is handed the ejected suite" \
    "test-other.sh" "$(seen_ejected sp-ej1)"

# ---------------------------------------------------------------------------
# THE DEFECT (sp-px6ng): a failed re-certification overwrites the EJECTED landstate
# record with RED, which used to be the ejected suites' only home. The sidecar
# _attr_eject also wrote must outlive that overwrite.
# ---------------------------------------------------------------------------
(
    export HOME="$HOMEDIR" SPIRA_CONF="$SPIRA_CONF_NONE" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB_NONE"
    . "$HERE/lib.sh"
    land_mark sp-ej1 RED deadbeef gate
)
is "landstate shows RED after the failed re-cert" \
    "RED" "$(cut -d' ' -f1 < "$RUN/landstate/sp-ej1" 2>/dev/null)"
is "reader: the ejected suite still reaches gate.sh's subprocess after RED" \
    "test-other.sh" "$(seen_ejected sp-ej1)"

echo
tl_summary
