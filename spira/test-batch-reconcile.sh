#!/usr/bin/env bash
#
# test-batch-reconcile.sh — batch.sh's pre-guard reconciliation: what a CERTIFIED,
#   LANDED, or RED/EJECTED landstate record becomes once branches move, land, or
#   disappear out from under it. One row per shape (sp-s088v.16, duplicate clusters
#   #2/#3): merged from test-batch-closed-red-live.sh, the reconcile-shaped cases of
#   test-batch.sh (6, 7, 8, A, B), and test-batch-stuck.sh's case g.
#
# sp-o7nbr: the CERTIFIED-orphan (no ref anywhere) and CERTIFIED-tip-already-landed
# reconcile sweeps this suite once covered (test-batch-certified-landed.sh,
# test-batch-certified-orphan.sh) are deleted from batch.sh — attribution is a batch
# state under the lifecycle machine now, not a landstate-file sweep run after the fact.
#
# NO BD. Every id below is answered from one static SPIRA_BDJSON_FIXTURE array —
# bdq routes through bdsim.py instead of a real bd process (lib.sh:bdq). The
# reconciliation loop's own logic reads bd exactly once per shape (_closed_red_live's
# `bdjson show`, and the priority sort before a new PR opens); the WARN-mail title
# lookups in main() call the bd binary directly (not through bdq) and are answered by
# a matching stub at $SPIRA_BD so a real store is never touched (law-prefer-the-real-
# dependency: bdsim.py already exists and is exercised against real bd once, in
# test-cockpit-bd-contract.sh — this file does not re-model it).
#
# tier: T1
# covers: spira/batch.sh spira/conf.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"
MAIL_LOG="$TMP/mail-log"
FORGE_LOG="$TMP/forge-log"
BD_FIXTURE="$TMP/bd.json"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
cp "$HERE/bdsim.py" "$SH/"   # bdq's SPIRA_BDJSON_FIXTURE seam looks beside its own lib.sh

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

cat > "$SH/mail.sh" <<MAIL
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$MAIL_LOG"
MAIL
chmod +x "$SH/mail.sh"

cat > "$SH/forge-fixture.sh" << FORGE
#!/usr/bin/env bash
cmd="\${1:-}"; shift; repo="\${1:-}"; shift
case "\$cmd" in
    main-gate-status) printf 'green deadbeef\n' ;;
    pr-create)
        n=\$(( \$(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "\$n" >> "$FORGE_LOG"
        cat >/dev/null
        printf '%s\n' "\$n"
        ;;
    pr-number|pr-comment|pr-list-queue|runs-active|runs-queue-branches) : ;;
    *) printf 'forge-fixture: unknown: %s\n' "\$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# One static bead fixture for every id any case below plants. bdjson (bdq's
# SPIRA_BDJSON_FIXTURE seam) answers `show` from this array — see lib.sh:bdq.
cat > "$BD_FIXTURE" <<'BDJSON'
[
  {"id":"sp-crl-stuck","title":"closed bead eviction race","status":"closed","labels":[]},
  {"id":"sp-crl-open","title":"open bead with red landstate","status":"open","labels":[]},
  {"id":"sp-crl-cert","title":"certified bead","status":"open","labels":[]},
  {"id":"sp-crl-ejected","title":"ejected closed bead","status":"closed","labels":[]},
  {"id":"sp-bt7-real","title":"real branch","status":"closed","labels":[]},
  {"id":"sp-btA-stale","title":"stale certification","status":"closed","labels":[]},
  {"id":"sp-g1","title":"post-landing false positive control","status":"closed","labels":[]}
]
BDJSON

# main()'s orphan/crl/lf title lookups call "$SPIRA_BD" directly (not through bdq),
# so they need their own stub answering the same fixture in the same shape.
cat > "$SH/bd-title-stub.sh" <<STUB
#!/usr/bin/env bash
shift 2   # drop "-C \$SPIRA_DB"
python3 "$SH/bdsim.py" "$BD_FIXTURE" "\$@"
STUB
chmod +x "$SH/bd-title-stub.sh"

batch() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="/nonexistent" \
    SPIRA_BD="$SH/bd-title-stub.sh" \
    SPIRA_BDJSON_FIXTURE="${_RC_BD_FIXTURE:-$BD_FIXTURE}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_BATCH_MAX="${_RC_BATCH_MAX:-8}" \
    SPIRA_QUEUE_BATCH_WAIT="${_RC_BATCH_WAIT:-0}" \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_STUCK_AGE="${_RC_STUCK_AGE:-}" \
        bash "$SH/batch.sh" "$@" 2>&1
}

clean_case() {
    rm -f "$QUEUEDIR/$REPONAME/open" "$RUN/queue-stuck-$REPONAME"
    : > "$MAIL_LOG"; : > "$FORGE_LOG"
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
}

mkbranch() {   # mkbranch <id> — a spira/<id> branch with one commit, tip printed
    local id="$1"
    git -C "$REPO" checkout -q -b "spira/$id" main
    printf '%s\n' "$id" > "$REPO/$id.txt"
    git -C "$REPO" add "$id.txt" && git -C "$REPO" commit -q -m "$id: work"
    git -C "$REPO" rev-parse "spira/$id"
    git -C "$REPO" checkout -q main
}

echo "test-batch-reconcile.sh"

# =============================================================================
# CLOSED + RED/EJECTED + LIVE BRANCH — the eviction-race stuck shape: no queue
# mechanism otherwise retrieves it (sp-htw4r). Open/CERTIFIED beads must not be
# reported (positive controls).
# =============================================================================
echo
echo "closed bead, RED/EJECTED landstate, live branch — the eviction-race shape:"
clean_case
_stuck_tip="$(mkbranch sp-crl-stuck)"
printf 'RED %s %s eviction-test\n' "$_stuck_tip" "$(date +%s)" > "$LANDSTATE/sp-crl-stuck"
_open_tip="$(mkbranch sp-crl-open)"
printf 'RED %s %s eviction-test\n' "$_open_tip" "$(date +%s)" > "$LANDSTATE/sp-crl-open"
_cert_tip="$(mkbranch sp-crl-cert)"
printf 'CERTIFIED %s %s certified\n' "$_cert_tip" "$(date +%s)" > "$LANDSTATE/sp-crl-cert"

out="$(batch "$REPONAME")"
is   "closed-red-live: batch exits 0" 0 "$?"
want "closed-red-live: WARN for sp-crl-stuck" "closed-red-live sp-crl-stuck" "$out"
want "closed-red-live: mailed the stuck-bead subject" "closed bead" "$(cat "$MAIL_LOG")"
nowant "positive control: open bead not reported"       "closed-red-live sp-crl-open" "$out"
nowant "positive control: CERTIFIED bead not reported"  "closed-red-live sp-crl-cert" "$out"
clean_case

echo
echo "EJECTED landstate is also detected (automated eviction shape):"
clean_case
mkbranch sp-crl-ejected >/dev/null
printf 'EJECTED faksha %s batch-ejection\n' "$(date +%s)" > "$LANDSTATE/sp-crl-ejected"
out="$(batch "$REPONAME")"
want "EJECTED shape is also logged" "closed-red-live sp-crl-ejected" "$out"
want "and mailed"                   "closed bead"                    "$(cat "$MAIL_LOG")"
clean_case

# =============================================================================
# LANDING-SHAPED RECORDS: landing.sh certifies with no trailing newline; `read`
# returns non-zero at EOF even after filling its variables — a reader using
# "|| continue" would silently skip every one of these.
# =============================================================================
echo
echo "landing-shaped records (no trailing newline) are still read as certified:"
clean_case
for i in $(seq 1 8); do
    tip="$(mkbranch "sp-bt6-$i")"
    printf '%s %s %s %s' CERTIFIED "$tip" "$(date +%s)" "" > "$LANDSTATE/sp-bt6-$i"
done
out6="$(batch "$REPONAME")"
want "landing-shaped records: all 8 counted certified" "8 certified" "$out6"
clean_case

# =============================================================================
# ALREADY-IN-BASE: certified tips equal to the base ref are marked LANDED and
# do not consume batch slots; only the one real branch is adopted.
# POSITIVE CONTROL: without the filter the 8 no-ops fill SPIRA_QUEUE_BATCH_MAX
# and the real branch is never included.
# =============================================================================
echo
echo "already-in-base: no-op certifications are LANDED, not batched:"
clean_case
NOW7="$(date +%s)"; OLD7=$(( NOW7 - 1800 - 2 ))
BASE_SHA7="$(git -C "$REPO" rev-parse origin/main)"
for i in $(seq 1 8); do
    git -C "$REPO" branch "spira/sp-bt7-noop-$i" main 2>/dev/null || true
    printf 'CERTIFIED %s %s\n' "$BASE_SHA7" $(( OLD7 - 1 )) > "$LANDSTATE/sp-bt7-noop-$i"
done
_real_tip="$(mkbranch sp-bt7-real)"
printf 'CERTIFIED %s %s\n' "$_real_tip" "$OLD7" > "$LANDSTATE/sp-bt7-real"

out7="$(batch "$REPONAME")"
want "already-in-base: only the real branch remains certified" "1 certified" "$out7"
is "already-in-base: sp-bt7-real landstate stays CERTIFIED" "CERTIFIED" \
    "$(awk '{print $1}' "$LANDSTATE/sp-bt7-real" 2>/dev/null)"
_all7=1
for i in $(seq 1 8); do
    _st7="" _rs7=""
    read -r _st7 _ _ _rs7 < "$LANDSTATE/sp-bt7-noop-$i" 2>/dev/null || true
    [ "$_st7" = "LANDED" ] && [ "$_rs7" = "already-in-base" ] || { _all7=0; break; }
done
is "already-in-base: 8 no-ops LANDED already-in-base" "1" "$_all7"
clean_case

# =============================================================================
# A. STALE-CERTIFICATION: a branch certified at the base tip then advanced must
#    NOT be marked LANDED (already-in-base); the new commit is re-certified at
#    its live tip.
# =============================================================================
echo
echo "stale-certification: advanced tip is re-certified, not marked LANDED:"
clean_case
NOW="$(date +%s)"; OLD_A=$(( NOW - 1800 - 1 ))
BASE_SHA_A="$(git -C "$REPO" rev-parse origin/main)"
git -C "$REPO" branch "spira/sp-btA-stale" main 2>/dev/null || true
printf 'CERTIFIED %s %s\n' "$BASE_SHA_A" "$OLD_A" > "$LANDSTATE/sp-btA-stale"
git -C "$REPO" worktree add -q "$RUN/worktree/sp-btA-stale" "spira/sp-btA-stale" 2>/dev/null || true
printf 'stale-cert test\n' > "$RUN/worktree/sp-btA-stale/stale.txt"
git -C "$RUN/worktree/sp-btA-stale" add -A
git -C "$RUN/worktree/sp-btA-stale" commit -q -m "sp-btA-stale: work after certification"
LIVE_TIP_A="$(git -C "$REPO" rev-parse "spira/sp-btA-stale")"

out_a="$(batch "$REPONAME")"
is "stale-cert: NOT marked LANDED" "0" \
    "$([ "$(awk '{print $1}' "$LANDSTATE/sp-btA-stale" 2>/dev/null)" = "LANDED" ] && echo 1 || echo 0)"
_tipA="$(awk '{print $2}' "$LANDSTATE/sp-btA-stale" 2>/dev/null)"
is "stale-cert: live tip in landstate" "$LIVE_TIP_A" "$_tipA"
want "stale-cert: logged"          "stale-certification"     "$out_a"
want "stale-cert: certified sha in log" "${BASE_SHA_A:0:8}"  "$out_a"
want "stale-cert: live sha in log"      "${LIVE_TIP_A:0:8}"  "$out_a"
git -C "$REPO" worktree remove -f "$RUN/worktree/sp-btA-stale" 2>/dev/null || true
clean_case

# =============================================================================
# B. STALE-CERT-IN-BASE: certified at a stale tip; the LIVE tip equals base —
#    LANDED (already-in-base), not batched (sp-n9z: no commits of its own, but
#    the CERTIFIED record predates the current base).
# =============================================================================
echo
echo "stale-cert-in-base: live tip already equals base — LANDED, not batched:"
clean_case
NOW_B2="$(date +%s)"; OLD_B2=$(( NOW_B2 - 1800 - 1 ))
git -C "$REPO" branch "spira/sp-btB-stale-base" main 2>/dev/null || true
STALE_TIP_B="0000000000000000000000000000000000000001"
printf 'CERTIFIED %s %s\n' "$STALE_TIP_B" "$OLD_B2" > "$LANDSTATE/sp-btB-stale-base"

out_b2="$(batch "$REPONAME")"
_st_b2=""; _rs_b2=""
read -r _st_b2 _ _ _rs_b2 < "$LANDSTATE/sp-btB-stale-base" 2>/dev/null || true
is "stale-cert-in-base: LANDED"                "LANDED"           "$_st_b2"
is "stale-cert-in-base: reason already-in-base" "already-in-base" "$_rs_b2"
is "stale-cert-in-base: no batch opened" "0" \
    "$([ -f "$QUEUEDIR/$REPONAME/open" ] && echo 1 || echo 0)"
want "stale-cert-in-base: logged" "stale-certification" "$out_b2"
clean_case

# =============================================================================
# test-batch-stuck::g — POST-LANDING FALSE POSITIVE (sp-wlt1r recurrence): a
# recent LANDED record (no trailing newline, land_mark format) plus an old
# CERTIFIED cert must NOT fire the stuck-queue alert. Before the fix, the
# no-newline LANDED record was skipped by a reader using "|| continue",
# last_moved stayed 0, and the fallback to oldest-cert-age fired the alert
# seconds after a landing.
# =============================================================================
echo
echo "post-landing false positive: recent LANDED + old CERTIFIED — no stuck mail:"
clean_case
_g1_tip="$(mkbranch sp-g1)"
printf 'CERTIFIED %s %s\n' "$_g1_tip" "$(( $(date +%s) - 7200 ))" > "$LANDSTATE/sp-g1"
printf 'LANDED none %s' "$(( $(date +%s) - 30 ))" > "$LANDSTATE/sp-g-landed"

outG="$(_RC_STUCK_AGE=300 _RC_BATCH_MAX=100 _RC_BATCH_WAIT=86400 batch "$REPONAME")"
nowant "post-landing: no stuck mail" "mailed operator" "$outG"
is    "post-landing: stuck flag absent" "0" \
    "$([ -f "$RUN/queue-stuck-$REPONAME" ] && echo 1 || echo 0)"
clean_case
tl_summary
