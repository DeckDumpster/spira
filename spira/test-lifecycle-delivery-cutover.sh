#!/usr/bin/env bash
#
# test-lifecycle-delivery-cutover.sh — the pr and push delivery machines wired into
# landing-pass (its pr_branch module, sp-t4y60) and landing.sh's push mode (sp-n1ilm),
# against a real throwaway spira_lifecycle database.
#
# WHAT THIS PROVES:
#   - PLANTED ABSENCE FIRST: with no delivery row at all — the ordinary case until the bead
#     machine's own cutover (sp-o7nbr) lands and starts creating them — every wrapper skips
#     quietly rather than fail, so passing the transitions below actually proves applying
#     the change, not merely surviving a call (law-absence-needs-a-positive-control).
#   - a squash-merged PR is delivered by content proof: a merge-tree comparison against the
#     merge commit, never the branch's own ancestry, because a squash merge rewrites every
#     SHA the branch carried and this fixture proves ancestry alone would say no;
#   - a PR closed unmerged is returned;
#   - a push that lands cleanly is delivered, proven by ancestry;
#   - a push rejected by a moved base is requeued;
#   - a push that genuinely conflicts is returned;
#   - a delivery already EXITED refuses a second event rather than firing it twice.
#
# host-reason: starts its own disposable `dolt sql-server`, the same shape
# test-lifecycle-container.sh already uses — testenv-batch.sh already provides the
# container this suite executes in.
#
# defect: sp-n1ilm
# tier: T2
# covers: spira-lc/src/callers.rs landing-pass/* lifecycle/* spira-lc/*
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

REPO="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"

# A PLAIN DIRECTORY, NEVER THIS CHECKOUT: SPIRA_HOME points here, so conf.sh's SPIRA_REPO
# derivation (git -C SPIRA_HOME rev-parse --show-toplevel) fails and falls back to this
# tmp dir's parent, never a real installed copy (law-gates-run-in-a-clean-environment).
SH="$TMP/spira"
mkdir -p "$SH"
cp "$HERE"/lib.sh "$HERE"/conf.sh "$HERE"/deps.toml \
   "$HERE"/suite-covers.sh "$SH/" 2>/dev/null
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "2>/dev/null/"

SPIRA_HOME="$SH"
export PATH="$PATH:$(dirname "$DOLT_BIN")"
unset SPIRA_LC_SOCKET

PORT=$((23000 + (RANDOM % 5000)))
SERVER_PID=""
cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

mkdir -p "$TMP/data"
cat > "$TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $PORT
  max_connections: 100
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML

"$DOLT_BIN" sql-server --config "$TMP/server.yaml" > "$TMP/server.log" 2>&1 &
SERVER_PID=$!
up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        up=1; break
    fi
    sleep 0.2
done
[ "$up" = 1 ] || bail "dolt sql-server never came up: $(cat "$TMP/server.log")"

root_sql() { "$DOLT_BIN" --data-dir "$TMP" --host 127.0.0.1 --port "$PORT" -u root -p "" --no-tls "$@"; }

# spira-lc is the tree under test's own build, found by name on the suite's PATH (sp-gypjk);
# lifecycle is switched on for this suite with SPIRA_LIFECYCLE_ENFORCE, never by a path.
command -v spira-lc >/dev/null 2>&1 || bail "spira-lc is not on PATH"
export SPIRA_LIFECYCLE_ENFORCE=1

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""

spira-lc admin-apply-ddl "$REPO/lifecycle/schema.sql" >"$TMP/schema.log" 2>&1
wantrc "schema applies cleanly" 0 $?
cat "$TMP/schema.log" >&2

# NOW source lib.sh — with SPIRA_HOME pointed at the plain fixture directory above.
SPIRA_HOME="$SH"
# shellcheck source=/dev/null
. "$SH/lib.sh"
is "spira-lc deliver reaches the lifecycle machine (switched on by SPIRA_LIFECYCLE_ENFORCE)" "1" "$SPIRA_LIFECYCLE_ENFORCE"

seed_bead() {       # seed_bead <id>
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO bead (bead_id, state, holds, version, updated_at) VALUES ('$1','READY','[]',0,0)" >/dev/null 2>&1
}
seed_delivery() {   # seed_delivery <id> <mode> <state> [pr]
    local id="$1" mode="$2" state="$3" pr="${4:-NULL}"
    seed_bead "$id"
    root_sql --use-db spira_lifecycle sql -q \
        "INSERT INTO delivery (bead_id, mode, state, pr, version) VALUES ('$id','$mode','$state',$pr,0)" >/dev/null 2>&1
}
delivery_state() {  # delivery_state <id> -> state
    root_sql --use-db spira_lifecycle sql -q "SELECT state FROM delivery WHERE bead_id='$1'" -r csv 2>/dev/null | tail -n1
}
delivery_merge_sha() {
    root_sql --use-db spira_lifecycle sql -q "SELECT IFNULL(merge_sha,'') FROM delivery WHERE bead_id='$1'" -r csv 2>/dev/null | tail -n1
}
last_event() {       # last_event <id> -> "<event> <applied>", or empty if none
    local out n
    out="$(root_sql --use-db spira_lifecycle sql -q \
        "SELECT event, applied FROM event WHERE lc_key='$1' AND machine='delivery' ORDER BY seq DESC LIMIT 1" -r csv 2>/dev/null)"
    n="$(printf '%s\n' "$out" | wc -l)"
    # csv prints the header row even with zero matches — one line means no data row.
    [ "$n" -ge 2 ] || return 0
    printf '%s\n' "$out" | tail -n1 | tr ',' ' '
}

# ── git fixture: a base branch and a bead branch, for the content-proof tests ─────────
GITREPO="$TMP/gitfix"
git init -q -b main "$GITREPO"
git -C "$GITREPO" config user.name t; git -C "$GITREPO" config user.email t@t
echo base > "$GITREPO/base.txt"; git -C "$GITREPO" add -A; git -C "$GITREPO" commit -q -m base

mk_pr_branch() {    # mk_pr_branch <id> -> leaves spira/<id> checked out with two commits on top of main
    local id="$1"
    git -C "$GITREPO" checkout -q main
    git -C "$GITREPO" checkout -q -b "spira/$id"
    echo "$id-b" > "$GITREPO/$id-b.txt"; git -C "$GITREPO" add -A; git -C "$GITREPO" commit -q -m "$id: b"
    echo "$id-c" > "$GITREPO/$id-c.txt"; git -C "$GITREPO" add -A; git -C "$GITREPO" commit -q -m "$id: c"
    git -C "$GITREPO" checkout -q main
}

squash_merge() {    # squash_merge <id> -> prints the merge commit sha on main
    local id="$1"
    git -C "$GITREPO" checkout -q main
    git -C "$GITREPO" merge -q --squash "spira/$id" >/dev/null
    git -C "$GITREPO" commit -q -m "squash merge $id"
    git -C "$GITREPO" rev-parse main
}

echo "test-lifecycle-delivery-cutover.sh"
echo

# --------------------------------------------------------------------------------------
# PLANTED ABSENCE: no delivery row exists yet (the state of the world until sp-o7nbr's
# bead-machine cutover lands and starts creating them). Every wrapper must skip, not fail.
# --------------------------------------------------------------------------------------
mk_pr_branch sp-norow
merge_sha_norow="$(squash_merge sp-norow)"
out="$(spira-lc deliver pr-merged "$GITREPO" sp-norow "spira/sp-norow" "$merge_sha_norow" 2>&1)"
rc=$?
wantrc "with no delivery row, lc_deliver_pr_merged skips rather than fails" 1 $rc
want   "and it says why" "no delivery row" "$out"
is     "and nothing was written to the event log" "" "$(last_event sp-norow)"

# --------------------------------------------------------------------------------------
# A SQUASH-MERGED PR IS DELIVERED BY CONTENT PROOF. Ancestry alone would say no — proven
# below — because a squash merge never records the branch as a parent; the merge-tree
# comparison content_landed performs is what lc_deliver_pr_merged actually relies on.
# --------------------------------------------------------------------------------------
mk_pr_branch sp-squash
seed_delivery sp-squash pr PR_OPEN 101
merge_sha="$(squash_merge sp-squash)"

git -C "$GITREPO" merge-base --is-ancestor "spira/sp-squash" "$merge_sha" 2>/dev/null
wantrc "POSITIVE CONTROL: ancestry alone cannot see a squash merge (proves content proof is load-bearing)" 1 $?

spira-lc deliver pr-merged "$GITREPO" sp-squash "spira/sp-squash" "$merge_sha"
wantrc "a squash-merged PR's delivery event applies" 0 $?
is "and the delivery row moves to EXITED" "EXITED" "$(delivery_state sp-squash)"
is "and the merge commit is recorded" "$merge_sha" "$(delivery_merge_sha sp-squash)"
is "and the event log names it Delivered, applied" "Delivered 1" "$(last_event sp-squash)"

out="$(spira-lc deliver pr-merged "$GITREPO" sp-squash "spira/sp-squash" "$merge_sha" 2>&1)"
wantrc "a delivery already EXITED refuses a second Delivered rather than firing it twice" 1 $?
want   "and says the row is no longer PR_OPEN" "not PR_OPEN" "$out"

# --------------------------------------------------------------------------------------
# A PR CLOSED UNMERGED IS RETURNED.
# --------------------------------------------------------------------------------------
seed_delivery sp-closed pr PR_OPEN 102
spira-lc deliver pr-closed sp-closed "closed unmerged"
wantrc "a closed-unmerged PR's delivery event applies" 0 $?
is "and the delivery row moves to EXITED" "EXITED" "$(delivery_state sp-closed)"
is "and the event log names it Returned, applied" "Returned 1" "$(last_event sp-closed)"

# --------------------------------------------------------------------------------------
# PUSH MODE: delivered on a clean push, requeued when the base moved with no real
# conflict, returned when it genuinely conflicts.
# --------------------------------------------------------------------------------------
seed_delivery sp-pushed push PUSHING
spira-lc deliver push-delivered sp-pushed deadbeefdeadbeefdeadbeefdeadbeefdeadbeef
wantrc "a clean push's delivery event applies" 0 $?
is "and the delivery row moves to EXITED" "EXITED" "$(delivery_state sp-pushed)"
is "and the pushed commit is recorded" "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" "$(delivery_merge_sha sp-pushed)"
is "and the event log names it Delivered, applied" "Delivered 1" "$(last_event sp-pushed)"

seed_delivery sp-requeued push PUSHING
spira-lc deliver push-requeued sp-requeued cafecafecafecafecafecafecafecafecafecafe
wantrc "a push rejected by a moved base — no real conflict — applies as requeued" 0 $?
is "and the delivery row moves to EXITED" "EXITED" "$(delivery_state sp-requeued)"
is "and the event log names it Requeued, applied" "Requeued 1" "$(last_event sp-requeued)"

seed_delivery sp-returned push PUSHING
spira-lc deliver push-returned sp-returned "genuinely conflicts with origin/main"
wantrc "a push that genuinely conflicts applies as returned" 0 $?
is "and the delivery row moves to EXITED" "EXITED" "$(delivery_state sp-returned)"
is "and the event log names it Returned, applied" "Returned 1" "$(last_event sp-returned)"

# --------------------------------------------------------------------------------------
# A GUARD THAT NEVER FIRES IS INDISTINGUISHABLE FROM ONE THAT WORKS: the wrong-mode guard
# is checked directly — a push-mode wrapper against a PR_OPEN row must refuse, not adapt.
# --------------------------------------------------------------------------------------
seed_delivery sp-wrongmode pr PR_OPEN 103
out="$(spira-lc deliver push-delivered sp-wrongmode abc123 2>&1)"
wantrc "a push wrapper against a PR_OPEN row refuses rather than adapting" 1 $?
want   "and says which state it found instead" "not PUSHING" "$out"
is     "and the row is untouched" "PR_OPEN" "$(delivery_state sp-wrongmode)"

tl_summary
