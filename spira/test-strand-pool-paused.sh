#!/usr/bin/env bash
#
# test-strand-pool-paused.sh — strand.sh suppresses 'starved' when the task pool is
#   deliberately set to zero (SPIRA_MAX_AEONS=0), and emits pool-paused instead.
#
# WHY THIS EXISTS. sp-2es5u: the operator set SPIRA_MAX_AEONS=0 to hold builders down
# while a merge-queue backlog drained. strand.sh reported "starved — escalate" and mailed
# the operator about a healthy timer, burying real alarms. A deliberate zero is not a
# fault (law-a-deliberate-state-is-not-a-fault); it must be reported as waiting, not
# starvation.
#
# THREE CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — POOL_PAUSED=0, ready beads, no live aeon → classifier
#      DOES produce "starved". Proves the check can fire before we trust its silence.
#
#   1. POOL PAUSED — POOL_PAUSED=1, same fixture → no "starved" in output;
#      "pool-paused" IS emitted as an info row (not escalate).
#
#   2. NO MAIL ON POOL-PAUSED — strand.sh check with a pool-paused info row in
#      the --from fixture does NOT call mail.sh, because info rows are skipped.
#
# PRE-FIX FAILURE (run against unfixed strand-classify.py):
#
#   FAIL  pool paused: pool-paused IS emitted: wanted [pool-paused] in [starved\t-\tescalate\t...]
#   FAIL  pool paused: starved NOT emitted: did not want [starved] in [starved\t-\tescalate\t...]
#
#   1 passed, 2 failed
#
# defect: sp-2es5u
# tier: T1
# covers: spira/strand-classify.py spira/strand.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"

BEADS='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'
READY='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'

classify() {
    local pool_paused="${1:-0}"
    printf '%s' "$BEADS" > "$TMP/beads.json"
    printf '%s' "$READY"  > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=0 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
    POOL_PAUSED="$pool_paused" \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-pool-paused.sh"

# ======================================================================================
echo
echo "case 0 — positive control: POOL_PAUSED=0 → classifier DOES produce starved:"
# ======================================================================================
out="$(classify 0)"
want "positive control: starved IS raised" "starved" "$out"

# ======================================================================================
echo
echo "case 1 — pool paused: POOL_PAUSED=1 → no starved escalation:"
# ======================================================================================
out="$(classify 1)"
want   "pool paused: pool-paused IS emitted"          "pool-paused"    "$out"
want   "pool paused: pool-paused has info disposition" "info"           "$out"
nowant "pool paused: starved NOT emitted"              "starved"        "$out"

# ======================================================================================
echo
echo "case 2 — no mail on pool-paused: strand.sh check skips info rows, sends no mail:"
# ======================================================================================
# A pool-paused row has info disposition. strand.sh check skips info rows (cmd_check
# reads disposition and continues on info), so no escalation reaches mail.sh.
printf 'pool-paused\t-\tinfo\t1 bead(s) ready but the task pool is set to zero: sp-example\taeons.sh pool <n>\n' \
    > "$TMP/fixture-pool-paused.tsv"
echo "sentinel ok" > "$TMP/run/sentinel.log"
MAIL_SENT="$TMP/mail-sent"
mkdir -p "$TMP/strand-home"
cat > "$TMP/strand-home/mail.sh" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = send ] || exit 0
shift
printf '%s\n' "$@" >> "$MAIL_SENT"
cat >> "$MAIL_SENT"
STUB
# MAIL_SENT must expand now, not in the stub at runtime.
sed -i "s|\$MAIL_SENT|$MAIL_SENT|g" "$TMP/strand-home/mail.sh"
chmod +x "$TMP/strand-home/mail.sh"

env \
    SPIRA_RUN="$TMP/run" \
    SPIRA_STRAND_GRACE=0 \
    SPIRA_LABELS=- \
    SPIRA_HOME="$TMP/strand-home" \
    bash "$HERE/strand.sh" check --from "$TMP/fixture-pool-paused.tsv" >/dev/null 2>&1

mail_sent="$(cat "$MAIL_SENT" 2>/dev/null || true)"
nowant "no mail: pool-paused info row does not trigger escalation" "operator" "$mail_sent"
tl_summary
