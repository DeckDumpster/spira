#!/usr/bin/env bash
#
# test-strand-capacity.sh — strand.sh suppresses 'starved' while the account is out of
#   capacity, and every escalation it files carries a --moot-when predicate.
#
#   ./test-strand-capacity.sh
#
# WHY THIS EXISTS. sp-9rscu: strand.sh classified a capacity pause (sentinel correctly
# declining to summon) as 'starved' and filed a needs-ryan escalation recommending that
# the operator check the sentinel timer — which was healthy. The condition clears on its
# own when the account reopens; it must not reach Ryan's queue at all. Additionally,
# every escalation strand.sh files lacked --moot-when, so even the correct escalations
# sat in the queue after their condition cleared, requiring manual dismissal.
#
# FOUR CASES (law-absence-needs-a-positive-control):
#
#   0. POSITIVE CONTROL — CAPACITY_PAUSED=0, ready beads, no live aeon → classifier
#      DOES produce "starved". Proves the check can fire before we trust its silence.
#
#   1. CAPACITY PAUSED — CAPACITY_PAUSED=1, same fixture → no "starved" in output;
#      "capacity-paused" IS emitted as an info row.
#
#   2. DETAIL CHECK — the capacity-paused row carries the CAPACITY_DETAIL text,
#      taken from the injected env var rather than re-derived from the log.
#
#   3. MOOT-WHEN CHECK — strand.sh check with a starved --from fixture and no
#      capacity pause → ask.sh is called with a --moot-when argument, so moot-sweep.sh
#      can withdraw the escalation without Ryan once the strand clears.
#
# PRE-FIX FAILURE (run against unfixed tree, 2026-09-10):
#
#   FAIL  capacity paused: capacity-paused IS emitted: wanted [capacity-paused] in [starved\t-\tescalate\t1 bead(s) ready and no live aeon: sp-example\tcheck spira-sentinel.timer and the tail of sentinel.log]
#   FAIL  capacity paused: starved NOT emitted: did not want [starved] in [starved\t-\tescalate\t1 bead(s) ready and no live aeon: sp-example\tcheck spira-sentinel.timer and the tail of sentinel.log]
#   FAIL  capacity detail: CAPACITY_DETAIL in row: wanted [out for another 1800s] in [starved\t-\tescalate\t...]
#   FAIL  --moot-when: ask.sh called with --moot-when: wanted [--moot-when] in []
#
#   1 passed, 4 failed
#
# defect: sp-9rscu
# tier: T1
# covers: spira/strand-classify.py spira/strand.sh
# hermetic-ok: no database, no systemd, no network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run"

# One ready bead, no live aeons — the fixture that starved fires on.
BEADS='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'
READY='[{"id":"sp-example","title":"test bead","status":"open","labels":["spira","plan"]}]'

classify() {
    local cap_paused="${1:-0}" cap_detail="${2:-}"
    printf '%s' "$BEADS" > "$TMP/beads.json"
    printf '%s' "$READY"  > "$TMP/ready.json"
    BEADS_FILE="$TMP/beads.json" \
    READY_FILE="$TMP/ready.json" \
    HOLDERS="" LIVE=0 GHOST_GRACE=300 \
    SPIRA_ASK_LABEL=needs-operator \
    CAPACITY_PAUSED="$cap_paused" \
    CAPACITY_DETAIL="$cap_detail" \
        python3 "$HERE/strand-classify.py"
}

echo "test-strand-capacity.sh"

# ======================================================================================
echo
echo "case 0 — positive control: CAPACITY_PAUSED=0 → classifier DOES produce starved:"
# ======================================================================================
# Without this half, a classifier that never fires looks correct when we test silence in
# case 1 (law-absence-needs-a-positive-control).
out="$(classify 0)"
want "positive control: starved IS raised" "starved" "$out"

# ======================================================================================
echo
echo "case 1 — capacity paused: CAPACITY_PAUSED=1 → no starved escalation:"
# ======================================================================================
# A capacity pause means the sentinel is correctly refusing to summon — not a strand.
# The row must not say "check spira-sentinel.timer" about a timer that is healthy.
out="$(classify 1 "the account is out for another 1800s")"
want   "capacity paused: capacity-paused IS emitted" "capacity-paused" "$out"
nowant "capacity paused: starved NOT emitted"        "starved"          "$out"

# ======================================================================================
echo
echo "case 2 — detail check: capacity-paused row carries CAPACITY_DETAIL text:"
# ======================================================================================
# The detail must name the capacity window and its source, taken from the injected
# CAPACITY_DETAIL env var rather than re-derived from the log (acceptance criterion 2).
out="$(classify 1 "out for another 1800s")"
want "capacity detail: CAPACITY_DETAIL in row" "out for another 1800s" "$out"

# ======================================================================================
echo
echo "case 3 — escalation: a starved strand fires mail.sh send:"
# ======================================================================================
# strand.sh must call mail.sh send operator when a strand fires so the operator is
# notified (acceptance criterion 3 — SPIRA_STRAND_GRACE=0 bypasses the grace window).
printf 'starved\t-\tescalate\t1 bead(s) ready and no live aeon: sp-example\tcheck sentinel\n' \
    > "$TMP/fixture.tsv"
echo "sentinel ok" > "$TMP/run/sentinel.log"
MAIL_SENT="$TMP/mail-sent"
mkdir -p "$TMP/strand-home"
# UNQUOTED HEREDOC so $MAIL_SENT expands now; \${1:-} and \$@ escape as script literals.
cat > "$TMP/strand-home/mail.sh" <<STUB
#!/usr/bin/env bash
[ "\${1:-}" = send ] || exit 0
printf '%s\n' "\$@" >> "$MAIL_SENT"
cat >> "$MAIL_SENT"
STUB
chmod +x "$TMP/strand-home/mail.sh"

env \
    SPIRA_RUN="$TMP/run" \
    SPIRA_STRAND_GRACE=0 \
    SPIRA_LABELS=- \
    SPIRA_HOME="$TMP/strand-home" \
    bash "$HERE/strand.sh" check --from "$TMP/fixture.tsv" >/dev/null 2>&1

mail_sent="$(cat "$MAIL_SENT" 2>/dev/null || true)"
want "escalation: mail.sh called for the starved escalation" "operator" "$mail_sent"
tl_summary
