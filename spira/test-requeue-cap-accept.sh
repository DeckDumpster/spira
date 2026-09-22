#!/usr/bin/env bash
#
# test-requeue-cap-accept.sh — acceptance counts (10-13 sends exactly one mail), the
# delivers:action exemption, counter non-conflation, and the poison path unchanged.
#
# Extracted from test-requeue-cap.sh to reduce the critical-path suite time.
#
# covers: spira/sentinel.sh spira/lib.sh
# timeout: 180
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
testdb_require test-requeue-cap-accept
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up requeue-cap-accept || {
    printf 'SKIP test-requeue-cap-accept: server testdb not available\n' >&2
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

seed_bead() {
    testdb_reset
    rm -rf "$RUN/requeue-asked" "$RUN/reclaim-asked" "$RUN/poison-asked"
    rm -f "$MAIL_LOG"; : > "$MAIL_LOG"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-11T00:00:00Z"}
{"id":"$1","title":"test bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-11T00:00:00Z"}
JSONL
}
cycle() {
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B update "$id" --status in_progress >/dev/null 2>&1
        B update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}
reopen_cycle() {
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B close "$id" --reason "done" >/dev/null 2>&1 || true
        B reopen "$id" >/dev/null 2>&1 || true
        i=$((i+1))
    done
}

echo "test-requeue-cap-accept.sh"

# ACCEPTANCE CASE: bead requeued at counts 10, 11, 12, 13 sends exactly one mail.
echo
echo "acceptance: counts 10..13 send exactly one mail total:"
seed_bead sp-rq-multi
reopen_cycle sp-rq-multi 10
sentinel >/dev/null 2>&1 || true
want "count 10 fires the first (and only) escalation" \
     "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"
reopen_cycle sp-rq-multi 1; : > "$MAIL_LOG"; sentinel >/dev/null 2>&1 || true
nowant "count 11 sends no mail (already asked per bead)" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"
reopen_cycle sp-rq-multi 1; : > "$MAIL_LOG"; sentinel >/dev/null 2>&1 || true
nowant "count 12 sends no mail" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"
reopen_cycle sp-rq-multi 1; : > "$MAIL_LOG"; sentinel >/dev/null 2>&1 || true
nowant "count 13 sends no mail" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

# delivers:action EXEMPTION
echo
echo "delivers:action — never triggers requeue cap:"
testdb_reset
rm -rf "$RUN/requeue-asked" "$RUN/reclaim-asked" "$RUN/poison-asked"
rm -f "$MAIL_LOG"; : > "$MAIL_LOG"
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-11T00:00:00Z"}
{"id":"sp-rq-act","title":"action bead","status":"open","issue_type":"task","labels":["spira","plan","delivers:action"],"updated_at":"2026-09-11T00:00:00Z"}
JSONL
reopen_cycle sp-rq-act 10
sentinel >/dev/null 2>&1 || true
nowant "delivers:action bead with 10 reopens sends no requeue mail" \
       "completed and requeued" "$(cat "$MAIL_LOG" 2>/dev/null || true)"

# COUNTERS DO NOT CONFLATE
echo
echo "counters do not conflate — bead with zero attempt events is never poisoned:"
seed_bead sp-no-poison
sentinel >/dev/null 2>&1 || true
nowant "a bead with no attempt events is never poisoned" \
       "spira-poison" "$(labels_of sp-no-poison)"

# POISON PATH UNCHANGED
echo
echo "poison path unchanged — bead at the attempt threshold is still poisoned:"
seed_bead sp-attempts
cycle sp-attempts 3
sentinel >/dev/null 2>&1 || true
want "a bead at the attempt threshold is still poisoned" "spira-poison" "$(labels_of sp-attempts)"

echo
echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
