#!/usr/bin/env bash
#
# test-poison-ask.sh — the ask is filed once per (bead, attempt count), ever; suppression
# is not the poison label. A closed-mid-pass bead is neither poisoned nor asked. An empty
# chamber poisons nothing and says so.
#
# Extracted from test-poison.sh to reduce the critical-path suite time.
#
# covers: spira/sentinel.sh spira/lib.sh spira/chamber/*
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-poison-ask
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — sentinel's poison threshold reads attempts_of via bd sql, which embedded mode refuses
export SPIRA_TESTDB_MODE=server
testdb_up poison-ask || {
    printf 'SKIP test-poison-ask: server testdb not available\n' >&2
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

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand.sh     'printf "%s" "${STRAND_OUT:-}"'
stub sending.sh    'printf "%s" "${SENDING_OUT:-}"'
stub gate.sh       'exit ${GATE_RC:-0}'
stub reflect.sh    'touch "$SPIRA_RUN/reflect.fired"'
stub mail.sh       '[ "${1:-}" = send ] || exit 0
printf "%s\n" "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
if [ -n "${ASK_CLOSES:-}" ]; then case "$*" in *"$ASK_CLOSES"*) ;;
    *) bd -C "$SPIRA_DB" close "$ASK_CLOSES" --reason landed >/dev/null 2>&1 ;; esac; fi'

printf 'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n'     > "$SH/chamber/t.fayth"
printf 'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_INCIDENT_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/tinc.fayth"

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
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" SPIRA_INFERENCE_EVERY=0 \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_SUMMON="$TMP/launch" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SKIP_CLOSED_CHECK=1 \
        bash "$SH/sentinel.sh" 2>&1
}

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))'; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
poisoned()    { [[ " $(labels_of "$1") " == *" spira-poison "* ]]; }
ispoisoned()  { poisoned "$2" && ok "$1" || bad "$1" "$2 was not poisoned"; }
notpoisoned() { poisoned "$2" && bad "$1" "$2 was poisoned" || ok "$1"; }

seed() {
    testdb_reset
    rm -rf "$RUN/poison-asked"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-block","title":"the blocker","status":"open","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-open","title":"blocked","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-open","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-open","depends_on_id":"sp-block","type":"blocks"}]}
JSONL
}

POISON_SEED=$(cat <<JSONL
{"id":"sp-orphan","title":"dispatchable, unparented","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-kid","title":"a child of the goal","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-young","title":"below the threshold","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
)
cycle() {
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B update "$id" --status in_progress >/dev/null 2>&1
        B update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}
seed_poison() {
    seed
    testdb_seed <<< "$POISON_SEED"
    cycle sp-orphan 3
    cycle sp-kid 3
    cycle sp-young 2
}

echo "test-poison-ask.sh"

# --------------------------------------------------------------------------------------
# THE ASK IS FILED ONCE PER (BEAD, ATTEMPT COUNT), EVER — AND ITS SUPPRESSION IS NOT THE
# POISON LABEL.
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"; out="$(sentinel)"
is "the first pass over the threshold asks exactly once" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"
out="$(sentinel)"
is "a second pass over the same count asks nothing more" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

B label remove sp-orphan spira-poison >/dev/null 2>&1
out="$(sentinel)"
ispoisoned "the bead is poisoned again, because it is still over the threshold" sp-orphan
is "but clearing the label did NOT re-arm the ask" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

B label remove sp-orphan spira-poison >/dev/null 2>&1
cycle sp-orphan 1
out="$(sentinel)"
is "a fourth attempt is a new fact and asks again" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*4 attempts' "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# A CLOSED BEAD NEVER POISONS AND NEVER ASKS.
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"
testdb_seed <<JSONL
{"id":"sp-late","title":"closed while the pass ran","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
cycle sp-late 3
out="$(ASK_CLOSES=sp-late sentinel)"
is          "the fixture really did close it mid-pass" "closed" "$(status_of sp-late)"
ispoisoned  "the bead that was still open is poisoned" sp-orphan
notpoisoned "the one that closed mid-pass is not"      sp-late
nowant "and the operator is not asked to drop landed work" "Spira bead sp-late" "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# AN EMPTY CHAMBER POISONS NOTHING AND SAYS SO.
# --------------------------------------------------------------------------------------
echo
seed_poison; out="$(ROSTER=nosuchfayth sentinel)"
notpoisoned "an empty chamber poisons nothing" sp-orphan
want "and says no bead is being examined" "no bead is dispatchable" "$out"

printf '\ntest-poison-ask.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
