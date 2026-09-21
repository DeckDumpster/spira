#!/usr/bin/env bash
#
# test-batch-closed-red-live.sh — batch.sh logs and mails the operator when a closed bead
#   has a RED/EJECTED landstate with a live branch (the eviction-race stuck shape).
#
# THE PROPERTY UNDER TEST (sp-htw4r). A bead closed by an in-flight aeon after its branch
# was evicted lands in a combination no queue mechanism retrieves: closed status, RED or
# EJECTED landstate, live branch. batch.sh runs on every queue cycle and is the right place
# to surface this before a "queue drained" conclusion stands.
#
# The counterpart _certified_orphans detects CERTIFIED landstate with no branch. This
# function (_closed_red_live) detects the inverse shape in the eviction-race case.
#
# TWO CASES, TWO OUTCOMES:
#
#   sp-crl-stuck:  closed bead, RED landstate, live branch.
#                  Batch must log WARN and mail the operator.
#
#   sp-crl-open:   open bead, RED landstate, live branch (not closed — not stuck).
#                  Must NOT be reported (positive control: the bead is claimable).
#
#   sp-crl-cert:   CERTIFIED landstate, live branch.
#                  Must NOT be reported (positive control: CERTIFIED is the normal queue state).
#
# SEEN RED WITHOUT THE FIX. _closed_red_live did not exist; all three cases were silent.
# The first case (closed+RED+live) produced no log line and no mail.
#
# defect: sp-htw4r
# covers: spira/batch.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-batch-closed-red-live
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up batchclosedredlive || { echo "test-batch-closed-red-live: could not build fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail-log"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

# mail.sh stub — records calls for assertion.
cat > "$SH/mail.sh" <<MAIL
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"
: > "$MAIL_LOG"

# Forge stub — records pr-create calls and returns PR numbers.
FORGE_LOG="$TMP/forge-log"
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" >> "$FORGE_LOG"
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"
: > "$FORGE_LOG"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX=8 \
    SPIRA_QUEUE_BATCH_WAIT=0 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
        bash "$SH/batch.sh" "$@" 2>&1
}

testdb_reset
testdb_seed <<JSONL
{"id":"sp-crl-stuck","title":"closed bead eviction race","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-crl-open","title":"open bead with red landstate","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
{"id":"sp-crl-cert","title":"certified bead","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
JSONL

# ─── Fixture: sp-crl-stuck — closed bead, RED landstate, live branch ──────────
git -C "$REPO" checkout -q -b spira/sp-crl-stuck main
printf 'work\n' > "$REPO/sp-crl-stuck.txt"
git -C "$REPO" add sp-crl-stuck.txt && git -C "$REPO" commit -q -m "sp-crl-stuck: work"
_stuck_tip="$(git -C "$REPO" rev-parse spira/sp-crl-stuck)"
git -C "$REPO" checkout -q main
printf 'RED %s %s eviction-test\n' "$_stuck_tip" "$(date +%s)" > "$LANDSTATE/sp-crl-stuck"

# ─── Fixture: sp-crl-open — open bead, RED landstate, live branch ─────────────
# Not stuck: the bead is open and claimable. _closed_red_live must not report it.
git -C "$REPO" checkout -q -b spira/sp-crl-open main
printf 'work\n' > "$REPO/sp-crl-open.txt"
git -C "$REPO" add sp-crl-open.txt && git -C "$REPO" commit -q -m "sp-crl-open: work"
_open_tip="$(git -C "$REPO" rev-parse spira/sp-crl-open)"
git -C "$REPO" checkout -q main
printf 'RED %s %s eviction-test\n' "$_open_tip" "$(date +%s)" > "$LANDSTATE/sp-crl-open"

# ─── Fixture: sp-crl-cert — open bead, CERTIFIED landstate, live branch ───────
# Normal queue state. Must not be reported.
git -C "$REPO" checkout -q -b spira/sp-crl-cert main
printf 'work\n' > "$REPO/sp-crl-cert.txt"
git -C "$REPO" add sp-crl-cert.txt && git -C "$REPO" commit -q -m "sp-crl-cert: work"
_cert_tip="$(git -C "$REPO" rev-parse spira/sp-crl-cert)"
git -C "$REPO" checkout -q main
printf 'CERTIFIED %s %s certified\n' "$_cert_tip" "$(date +%s)" > "$LANDSTATE/sp-crl-cert"

echo "test-batch-closed-red-live.sh"

out="$(batch "$REPONAME")"
rc=$?

# =============================================================================
echo
echo "closed+RED+live detection — sp-crl-stuck is logged and mailed:"
# =============================================================================
is   "batch exits 0"                                0 "$rc"
want "batch logs WARN for closed-red-live"          "closed-red-live sp-crl-stuck" "$out"
want "mail.sh was called with the stuck-bead subject" \
    "closed bead" "$(cat "$MAIL_LOG" 2>/dev/null)"

# =============================================================================
echo
echo "positive controls — open bead and CERTIFIED bead are not reported:"
# =============================================================================
nowant "open bead with RED not reported" "closed-red-live sp-crl-open" "$out"
nowant "CERTIFIED bead not reported"     "closed-red-live sp-crl-cert" "$out"

# =============================================================================
echo
echo "EJECTED landstate also detected (automated eviction shape):"
# =============================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-crl-ejected","title":"ejected closed bead","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"2026-09-21T00:00:00Z"}
JSONL
git -C "$REPO" checkout -q -b spira/sp-crl-ejected main
printf 'ejected\n' > "$REPO/sp-crl-ejected.txt"
git -C "$REPO" add sp-crl-ejected.txt && git -C "$REPO" commit -q -m "sp-crl-ejected: work"
git -C "$REPO" checkout -q main
printf 'EJECTED faksha %s batch-ejection\n' "$(date +%s)" > "$LANDSTATE/sp-crl-ejected"
: > "$MAIL_LOG"; : > "$FORGE_LOG"
out2="$(batch "$REPONAME")"
want "EJECTED shape is also logged"    "closed-red-live sp-crl-ejected" "$out2"
want "and mailed"                      "closed bead"                    "$(cat "$MAIL_LOG")"

printf '\n%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
