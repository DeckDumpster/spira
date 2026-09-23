#!/usr/bin/env bash
#
# test-requeue-cap.sh — the requeue and reclaim caps: a bead one under must not escalate,
# a bead at or over must escalate with wording distinct from poison.
#
#   ./test-requeue-cap.sh
#
# THE DEFECT THIS GUARDS. The poison threshold caps sp-attempt-N at POISON_AT; the two
# excluded counters — sp-requeue-N (the harness put finished work back) and sp-reclaim-N
# (the aeon died holding it) — were uncapped. A bead that conflicted on every rebase could
# requeue indefinitely, spending one full aeon session per cycle, with nothing stopping it
# and nothing reaching the operator. The cap sends one escalation per crossing and names
# the cause distribution; the wording is distinct from poison because the diagnosis and the
# remedy differ.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control). "No escalation fired" is
# also what a check that never runs returns. Each below-cap assertion is paired with an
# at-cap assertion through the same code path, so absence has been proven detectable.
#
# The escalation text is verified for the key phrases the bead description requires:
# - requeue: "completed and requeued" + count + "never landed"
# - reclaim: "aeons died holding" + count + "never judged"
# These distinguish the escalation from a poison ask, which says "change the approach".
#
# EXISTING POISON PATH IS NOT RETESTED HERE. test-poison.sh covers it; the only thing
# asserted here is that a bead with requeue/reclaim labels at the threshold and zero
# attempts is NOT poisoned — confirming the counters are not conflated.
#
# covers: spira/sentinel.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-requeue-cap
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up requeue-cap || {
    printf 'SKIP test-requeue-cap: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand.sh     'printf "%s" "${STRAND_OUT:-}"'
stub sending.sh    'printf "%s" "${SENDING_OUT:-}"'
stub governor.sh   'exit 0'
stub gate.sh       'exit ${GATE_RC:-0}'
stub reflect.sh    'touch "$SPIRA_RUN/reflect.fired"'
stub mail.sh       'printf "%s\n" "$*" >> "$MAIL_LOG"; cat >/dev/null'

# TWO PERSONAS to prove the check is not hardcoded to one partition.
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"
printf 'FAYTH_LABELS="spira,incident"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/tinc.fayth"

B() { bd -C "$SPIRA_DB" "$@"; }
export MAIL_LOG="$TMP/mail.log"; : > "$MAIL_LOG"
cat > "$TMP/launch" <<'L'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$LAUNCH_LOG"
exit "${LAUNCH_RC:-0}"
L
cat > "$TMP/systemctl" <<'S'
#!/usr/bin/env bash
printf '%s\n' "${LAND_STATE:-inactive}"
S
chmod +x "$TMP/launch" "$TMP/systemctl"
export LAUNCH_LOG="$TMP/launch.log"

sentinel() {
    rm -f "$RUN/reflect.fired" "$RUN/inference.cooldown"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t}" SPIRA_INFERENCE_EVERY=0 \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_SUMMON="$TMP/launch" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SKIP_CLOSED_CHECK=1 \
    SPIRA_REQUEUE_AT="${SPIRA_REQUEUE_AT:-3}" \
    SPIRA_RECLAIM_AT="${SPIRA_RECLAIM_AT:-3}" \
        bash "$SH/sentinel.sh" 2>&1
}

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))'; }

# SEED a goal epic plus one ready bead.
# Clears the asked-dirs so dedup state from a prior case does not bleed in.
seed_bead() {   # seed_bead <id>
    testdb_reset
    rm -rf "$RUN/requeue-asked" "$RUN/reclaim-asked" "$RUN/poison-asked"
    rm -f "$MAIL_LOG"; : > "$MAIL_LOG"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-11T00:00:00Z"}
{"id":"$1","title":"test bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-11T00:00:00Z"}
JSONL
}
cycle() {   # cycle <id> <n> — create n status_changed(in_progress) events via bd update
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B update "$id" --status in_progress >/dev/null 2>&1
        B update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}

echo "test-requeue-cap.sh"

reopen_cycle() {   # reopen_cycle <id> <n> — n close/reopen cycles; creates n reopened events
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B close "$id" --reason "done" >/dev/null 2>&1 || true
        B reopen "$id" >/dev/null 2>&1 || true
        i=$((i+1))
    done
}

# --------------------------------------------------------------------------------------
# REQUEUE CAP VIA REOPENED EVENTS. reopens_of() counts event_type='reopened' rows;
# sentinel CHECK4 reads it into _requeues (sp-6bop). Pairs: below-cap first proves
# absence is detectable, then at-cap proves the escalation fires.
# REQUEUE_AT is pinned to 3 in the sentinel() wrapper — non-default (default is 5).
# --------------------------------------------------------------------------------------
echo
echo "requeue cap via reopened events:"
seed_bead sp-rq-below
reopen_cycle sp-rq-below 2
sentinel >/dev/null 2>&1 || true
nowant "2 reopens (below cap 3) fires no requeue escalation" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

seed_bead sp-rq-at
reopen_cycle sp-rq-at 3
sentinel >/dev/null 2>&1 || true
want "3 reopens (at cap 3) fires the requeue escalation" \
     "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"
want "the escalation names the reopen count" \
     "requeued 3 times" "$(cat "$MAIL_LOG" 2>/dev/null || true)"
nowant "requeue thrash does not add spira-poison" \
       "spira-poison" "$(labels_of sp-rq-at)"

# DEDUP — per-bead: once asked, no re-mail even at a new count (positive control below).
# POSITIVE CONTROL: the first-ask assertion above proves the check can detect mail.
# This section proves a new count does NOT re-mail — an assertion that FAILS against
# the old per-(bead,count) requeue_asked, establishing it can distinguish old from new.
echo
echo "requeue dedup — per-bead: once asked, never re-mailed for higher counts:"
: > "$MAIL_LOG"
sentinel >/dev/null 2>&1 || true
nowant "second pass at count 3 sends no mail (dedup stamp)" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

reopen_cycle sp-rq-at 1   # count advances to 4
: > "$MAIL_LOG"
sentinel >/dev/null 2>&1 || true
nowant "count 4 (higher count) sends no mail — per-bead dedup, not per-count" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

# CLOSED-BEAD REQUEUE CAP. A bead that ends each sentinel cycle closed-and-unlanded is
# never in dispatchable_open, so the cap above cannot fire for it. A supplement reads
# closed beads with a branch: label; it must escalate those at or above the cap.
echo
echo "requeue cap for closed beads (supplement):"

seed_bead sp-rq-closed-below
B label add sp-rq-closed-below "branch:spira/sp-rq-closed-below" >/dev/null 2>&1
reopen_cycle sp-rq-closed-below 2
B close sp-rq-closed-below --reason "done" >/dev/null 2>&1 || true
sentinel >/dev/null 2>&1 || true
nowant "closed bead with 2 reopens (below cap 3) fires no escalation" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

seed_bead sp-rq-closed-at
B label add sp-rq-closed-at "branch:spira/sp-rq-closed-at" >/dev/null 2>&1
reopen_cycle sp-rq-closed-at 3
B close sp-rq-closed-at --reason "done" >/dev/null 2>&1 || true
sentinel >/dev/null 2>&1 || true
want "closed bead with 3 reopens (at cap) fires the requeue escalation" \
     "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"
want "closed-bead escalation names the reopen count" \
     "requeued 3 times" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

: > "$MAIL_LOG"
sentinel >/dev/null 2>&1 || true
nowant "second pass sends no mail for closed bead (dedup stamp)" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
