#!/usr/bin/env bash
#
# test-czar-partition.sh — czar.fayth must claim only czar-trigger beads, not plan beads.
#
# THE DEFECT THIS GUARDS. A czar that claims plan beads becomes a second builder and stops
# watching the queue. The partition predicate must be FAYTH_LABELS matching ONLY
# SPIRA_CZAR_LABEL, not SPIRA_PLAN_LABEL or SPIRA_INCIDENT_LABEL.
#
# WHAT THIS SUITE CHECKS.
#   1. czar.fayth FAYTH_LABELS contains SPIRA_CZAR_LABEL (default: czar-trigger).
#   2. czar.fayth FAYTH_LABELS does NOT contain plan or incident.
#   3. fayth_ready czar, over a ready set holding one czar-trigger bead and one plan bead,
#      takes the czar-trigger bead and never the plan bead (the end-to-end predicate check).
#   4. builder.fayth FAYTH_LABELS does NOT contain czar-trigger — partitions are disjoint.
#
# WHAT THIS SUITE DOES NOT USE. No real database, no systemd, no network. `fayth_ready` is a
# one-line shim onto `spira-claim fayth-ready` (wave 4.25, sp-obhv6), and its ready set is the
# lifecycle machine's READY rows (sp-v62vn: no off mode, so no `bd ready` argv to read): a
# spira-lc stand-in (testlib `lc_mirror_bd`) answers `list` from a fake `$SPIRA_BD`'s fixture
# beads, and spira-claim's own label predicate does the partitioning — the same fixture
# test-builder-qa-proposed.sh uses.
#
# tier: T1
# covers: spira/chamber/czar.fayth spira/lib.sh spira/conf.sh spira-claim/* UC-dispatch-09 UC-config-store-preflight-15
# hermetic-ok: no real database, no systemd; SPIRA_BD and SPIRA_SUMMON are stubs
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

. "$HERE/testlib.sh"
lack() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/run"

export SPIRA_HOME="$HERE"
export SPIRA_CONF="$T/no-such.conf"
# The complete fixture declares a non-empty SPIRA_CHAMBER; nothing derives it from
# SPIRA_HOME any more (sfail round 2, pattern 6) — without this, czar.fayth/builder.fayth
# are never found in the real chamber this suite reads.
tl_config SPIRA_RUN="$T/run" SPIRA_CHAMBER="$HERE/chamber"
# shellcheck disable=SC1090
. "$HERE/lib.sh"

echo "test-czar-partition.sh"

# ==========================================================================================
echo
echo "czar.fayth — FAYTH_LABELS contains czar-trigger"
# ==========================================================================================
czar_labels="$(fayth_get czar FAYTH_LABELS)"
want "czar FAYTH_LABELS contains SPIRA_CZAR_LABEL (czar-trigger)" \
    "${SPIRA_CZAR_LABEL:-czar-trigger}" "$czar_labels"

# ==========================================================================================
echo
echo "czar.fayth — FAYTH_LABELS does not contain plan or incident (no partition bleed)"
# ==========================================================================================
lack "czar FAYTH_LABELS does not contain plan" "plan" "$czar_labels"
lack "czar FAYTH_LABELS does not contain incident" "incident" "$czar_labels"

# ==========================================================================================
echo
echo "fayth_ready czar — the czar's ready set is the czar-trigger bead, never the plan bead"
# ==========================================================================================
# Both beads are READY in the machine and carry the scope label; one is a queue-state
# escalation (czar-trigger), the other plain plan work. czar.fayth's own FAYTH_LABELS is the
# only thing that may tell them apart.
CZ="${SPIRA_CZAR_LABEL:-czar-trigger}"
FIXTURE="$T/beads.json"
cat > "$FIXTURE" <<EOF
[{"id":"sp-czq1","title":"queue-state escalation","status":"open","issue_type":"task","priority":1,"labels":["spira","$CZ"]},
 {"id":"sp-czp1","title":"plan work","status":"open","issue_type":"task","priority":1,"labels":["spira","plan"]}]
EOF
FAKE_BD="$T/fake-bd"
cat > "$FAKE_BD" <<EOF
#!/bin/sh
case " \$* " in *" list "*) cat "$FIXTURE" ;; *) echo '[]' ;; esac
EOF
chmod +x "$FAKE_BD"
lc_mirror_bd "$T/lc"

# SPIRA_SCOPE_LABEL pinned to the fixture's own scope label, so czar.fayth's FAYTH_LABELS
# (scope + czar label) is a predicate both fixture beads could satisfy but for the czar label.
# SPIRA_BD is declared BOTH ways: spira-claim itself resolves it via cfg() (tl_config), but
# the spira-lc stand-in lc_mirror_bd installs is a plain bash stub reading ${SPIRA_BD:-bd}
# straight from its own inherited env, not through config at all — it needs the real export.
tl_config SPIRA_BD="$FAKE_BD" SPIRA_SCOPE_LABEL=spira
czar_count="$(PATH="$T/lc:$PATH" SPIRA_BD="$FAKE_BD" SPIRA_DB="/fake/db" \
    fayth_ready czar 2>/dev/null)"
czar_set="$(PATH="$T/lc:$PATH" SPIRA_BD="$FAKE_BD" SPIRA_DB="/fake/db" \
    _spira_claim fayth-ready czar --json 2>/dev/null)"
# POSITIVE CONTROL: the fixture reaches spira-claim — exactly one bead counts.
is     "positive control: fayth_ready czar counts exactly one bead" "1" "$czar_count"
want   "fayth_ready czar takes the czar-trigger bead" "sp-czq1" "$czar_set"
nowant "fayth_ready czar does NOT take the plan bead" "sp-czp1" "$czar_set"

# ==========================================================================================
echo
echo "builder.fayth — FAYTH_LABELS does not contain czar-trigger (partitions are disjoint)"
# ==========================================================================================
# Proves the partitions cannot race on the same bead: a czar-trigger bead is not in the
# builder's predicate, so the czar has exclusive ownership of queue-state events.
builder_labels="$(fayth_get builder FAYTH_LABELS)"
lack "builder FAYTH_LABELS does not contain czar-trigger" \
    "${SPIRA_CZAR_LABEL:-czar-trigger}" "$builder_labels"

# ==========================================================================================
echo
echo "czar.fayth — FAYTH_LANE is set (draws from its own capacity, not the task pool)"
# ==========================================================================================
czar_lane="$(fayth_get czar FAYTH_LANE)"
want "czar FAYTH_LANE is set to czar" "czar" "$czar_lane"

echo
tl_summary
