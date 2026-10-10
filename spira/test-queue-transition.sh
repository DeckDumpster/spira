#!/usr/bin/env bash
#
# test-queue-transition.sh — sp-aelyk: row 6 of the local/main design, the transition
# command between queue.local and queue.forge. `queue.sh to-forge` holds the repo's queue
# lock for the whole move, runs a final publish, requires it green on the forge, verifies
# origin/main and local/main are then identical, flips the repo-map row to
# queue.forge | origin/main, and archives local/main at refs/archive/local/main.
# `queue.sh to-local` is the reverse: it re-derives local/main from the forge base (syncing
# to its current tip first) and flips the row back.
#
# FOUR PROPERTIES FROM THE BEAD'S ACCEPTANCE, each with a positive control:
#
#   1. A fixture transition leaves origin/main == local/main and the row at queue.forge,
#      whether or not there was anything left to publish.
#   2. It refuses, changing NOTHING, when the final publish comes back red — the row stays
#      queue.local, local/main is untouched, the forge is untouched, and the publish record
#      is left open for the ordinary fix-forward recovery (proved by settling it green
#      afterward and confirming the retried transition then succeeds).
#   3. It refuses, changing NOTHING, when the forge has diverged from local/main (something
#      pushed to it outside the publish queue) — the same "refs differ" precondition.
#   4. The reverse flip works, and syncs the new local/main to the forge's CURRENT tip, not
#      a stale one — proved by advancing the forge further while in queue.forge mode before
#      flipping back.
#
# tier: T1
# covers: queue/src/* spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
. "$HERE/testlib/lc-fixture.sh"
testdb_require test-queue-transition
TMP="$(mktemp -d)"
trap 'lcfix_down; testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up qtrans || { echo "test-queue-transition: could not build a fixture database"; exit 1; }
lcfix_up || { echo "test-queue-transition: could not build a lifecycle fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-queue-transition.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SH/"
chmod +x "$SH"/*.sh 2>/dev/null || true
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub mail 'exit 0'

REMOTE="$TMP/remote.git"; REPO="$TMP/repo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
timeout 5 git -C "$REPO" push -q origin main
git -C "$REPO" branch local/main main

RUN="$TMP/run"; QDIR="$RUN/queue"; REPONAME=fixtrans; RELEASES="$TMP/releases"
mkdir -p "$RUN/worktree" "$QDIR" "$RELEASES"
RMAP="$TMP/repo-map"
printf '%s | %s | queue.local | local/main | | |\n' "$REPONAME" "$REPO" > "$RMAP"

# Forge stub — same shape as test-queue-publish.sh's: CALL_LOG proves what was actually
# asked of it, FIXTURE_CHECK_STATUS drives check-status's answer.
CALL_LOG="$TMP/call-log"; : > "$CALL_LOG"
PRSEQ="$TMP/pr-seq"
export CALL_LOG PRSEQ
cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift  # drop the repo-dir arg every verb takes
case "$cmd" in
    pr-create)
        n=$(( $(cat "$PRSEQ" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n" > "$PRSEQ"
        printf 'pr-create\n' >> "$CALL_LOG"
        printf '%s\n' "$n"
        ;;
    pr-state)
        printf 'pr-state\n' >> "$CALL_LOG"
        printf 'open\n'
        ;;
    check-status)
        printf 'check-status\n' >> "$CALL_LOG"
        printf '%s\n' "${FIXTURE_CHECK_STATUS:-green}"
        ;;
    pr-close)
        printf 'pr-close %s\n' "$1" >> "$CALL_LOG"
        ;;
    *) printf '%s\n' "$cmd" >> "$CALL_LOG" ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

export SPIRA_CONF=/nonexistent
export SPIRA_HOME="$SH"
export SPIRA_REPO="$REPO"
# queue/DESIGN.md §8 D12: these hand-built heads were never gated; the named override lands them.
export SPIRA_LAND_UNGATED="fixture: hand-built heads no gate judged"
tl_config SPIRA_HOME_REPO="$REPONAME" SPIRA_RUN="$RUN" SPIRA_QUEUE_DIR="$QDIR" \
    SPIRA_REPO_MAP="$RMAP" SPIRA_FORGE="$SH/forge-fixture.sh" SPIRA_RELEASES="$RELEASES" \
    SPIRA_QUEUE_TRANSITION_POLLSEC=1 SPIRA_QUEUE_TRANSITION_MAXSEC=5
# queue.sh's own `agrees()` (transition.rs) refuses unless the legacy repo-map row and
# spira.toml's repo.<name>.{mode,base} already match — the complete fixture declares no
# [repo.fixtrans] at all, reading as mode="" base="" ("already disagree"). Declare this
# suite's own row, matching $RMAP's initial queue.local|local/main exactly; queue.sh's own
# transitions keep it in sync afterward by writing the same (writable, last-layer) file.
# THREE SEPARATE `spira-config set` calls do not work here: `path` and `mode` are both
# mandatory, non-Option fields of [repo.<name>] (spira-config/src/lib.rs RepoSection), and
# every `set` re-validates the WHOLE document before writing — a call setting only one of
# them leaves the table with the other missing, so write_doc's validate() refuses every one
# of the three in turn (silently: stdout was empty, and the real error was on stderr, above
# the TAP output this harness's tail-only capture never showed). Append the complete table
# directly instead, so every field exists from the first read.
cat >> "$_TL_CONF_OVERRIDE" <<REPOFIXTRANS
[repo.fixtrans]
path = "$REPO"
mode = "queue.local"
base = "local/main"
REPOFIXTRANS

queue() {
    FIXTURE_CHECK_STATUS="${CHECK_STATUS:-green}" \
        SPIRA_HOME="$SH" command queue "$@" 2>&1
}
verdict() {
    FIXTURE_CHECK_STATUS="${CHECK_STATUS:-green}" \
        SPIRA_HOME="$SH" command queue verdict "$REPONAME" 2>&1
}
# mk_bins <head> — land-local refuses without a --with-bins corpus for the tree it is
# landing (sp-sf60f); every head this suite lands needs one.
mk_bins() {
    local head="$1" tree dir
    tree="$(git -C "$REPO" rev-parse "${head}^{tree}")"
    dir="$RUN/cargo-target-bins/$tree/release"
    mkdir -p "$dir"
    printf 'fake\n' > "$dir/fakebin"
    chmod +x "$dir/fakebin"
}

# land <id> <file> <content> — one round as the batcher builds it: the member's work commit,
# merged --no-ff onto local/main under the queue's own land subject ("spira: land <id>",
# queue/DESIGN.md §8 D4 — publish, and so to-forge's final publish, reads its members from
# those commits), then land-local'd.
land() {
    local id="$1" file="$2" content="$3"
    git -C "$REPO" checkout -qb "work-$id" local/main
    printf '%s\n' "$content" > "$REPO/$file"
    git -C "$REPO" add "$file"
    git -C "$REPO" commit -q -m "$id: the work"
    local tip; tip="$(git -C "$REPO" rev-parse "work-$id")"
    git -C "$REPO" checkout -qb "round-$id" local/main
    git -C "$REPO" merge -q --no-ff -m "spira: land $id" "work-$id"
    local head; head="$(git -C "$REPO" rev-parse "round-$id")"
    git -C "$REPO" checkout -q main
    git -C "$REPO" branch -D "round-$id" "work-$id" >/dev/null 2>&1
    mk_bins "$head"
    lcfix_seed "$id" CERTIFIED "$tip"
    queue land-local "$REPONAME" --head "$head" --members "$id:$tip" >/dev/null
}

B() { bd -C "$SPIRA_DB" "$@"; } # batch-job: fixture bd call against the suite's throwaway store
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {   # seed <id>
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":["plan","repo:%s","spira-submitted"],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "$REPONAME" | testdb_seed
}
remote_main()   { git -C "$REMOTE" rev-parse main 2>/dev/null; }
localmain()     { git -C "$REPO" rev-parse --verify -q local/main 2>/dev/null; }
row()           { grep "^$REPONAME " "$RMAP"; }
callcount()     { grep -c "^$1" "$CALL_LOG" 2>/dev/null; }
clear_calls()   { : > "$CALL_LOG"; }
publish_file()  { cat "$QDIR/$REPONAME/publish" 2>/dev/null; }

# passed — the full-suite local pass a green round records for its head (sp-x334k): publish refuses a
# local/main head without one, so every publish here stands on the pass production would have.
passed() { local o; o="$(spira-config local-pass record full-suite "$(git -C "$REPO" rev-parse local/main)" fixture-round 2>&1)" || echo "# passed() FAILED: $o" >&2; }

# landmode <name> -> "<repo_land> <spira_landref>", read fresh out of the CURRENT repo-map —
# a subshell sourcing the copied lib.sh under the exact same env the commands above use, so
# an assertion never trusts its own memory of what it just wrote.
landmode() { ( . "$SH/lib.sh" >/dev/null 2>&1; printf '%s %s\n' "$(repo_land "$1")" "$(spira_landref "$1" 2>/dev/null)" ); }

# ============================================================================
echo
echo "1 — to-forge on a repo with nothing new to publish still flips the row"
# ============================================================================
BASE0="$(remote_main)"
passed; out="$(queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "1: to-forge succeeds" || bad "1: to-forge succeeds" "got rc=$rc out=$out"
want "1: names the new mode" "queue.forge" "$out"
want "1: repo-map land column now reads queue.forge" "queue.forge" "$(row)"
want "1: repo-map base column now reads origin/main" "origin/main" "$(row)"
is "1: repo_land now normalizes to queue" "queue origin/main" "$(landmode "$REPONAME")"
is "1: production is unchanged (nothing was ever published)" "$BASE0" "$(remote_main)"
git -C "$REPO" show-ref --verify --quiet refs/heads/local/main \
    && bad "1: local/main is gone from refs/heads" "still present" \
    || ok "1: local/main is gone from refs/heads"
ARCH0="$(git -C "$REPO" rev-parse -q --verify refs/archive/local/main)"
is "1: local/main is archived at its old tip" "$BASE0" "$ARCH0"

# ============================================================================
echo
echo "2 — reverse flip (to-local) recreates local/main from the forge"
# ============================================================================
out="$(queue to-local "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "2: to-local succeeds" || bad "2: to-local succeeds" "got rc=$rc out=$out"
is "2: repo_land is back to queue.local" "queue.local local/main" "$(landmode "$REPONAME")"
want "2: repo-map base column reads local/main" "local/main" "$(row)"
is "2: local/main is recreated at the forge's tip" "$BASE0" "$(localmain)"

# ============================================================================
echo
echo "3 — a green final publish flips the row and makes origin/main == local/main"
# ============================================================================
seed sp-tr1
land sp-tr1 one.txt one
HEAD1="$(localmain)"
nowant "3 setup: production has not moved yet" "$HEAD1" "$(remote_main)"

passed; out="$(CHECK_STATUS=green queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "3: to-forge succeeds on a green publish" || bad "3: to-forge succeeds on a green publish" "got rc=$rc out=$out"
is "3: production (origin/main) now equals local/main's old tip EXACTLY" "$HEAD1" "$(remote_main)"
is "3: repo_land now reads queue at origin/main" "queue origin/main" "$(landmode "$REPONAME")"
[ "$(callcount pr-create)" -eq 1 ] && ok "3: the publish PR was opened" || bad "3: the publish PR was opened" "$(cat "$CALL_LOG")"
[ "$(callcount pr-close)" -eq 1 ] && ok "3: the publish PR was closed on settle" || bad "3: the publish PR was closed on settle" "$(cat "$CALL_LOG")"
[ ! -f "$QDIR/$REPONAME/publish" ] && ok "3: the publish record is retired" \
    || bad "3: the publish record is retired" "got: $(publish_file)"
is "3: the member bead is still closed from the local landing, not re-touched" \
    closed "$(field sp-tr1 status)"

# back to queue.local for the next cases
queue to-local "$REPONAME" >/dev/null

# ============================================================================
echo
echo "4 — a red final publish refuses the transition, changing nothing"
# ============================================================================
clear_calls
seed sp-tr2
land sp-tr2 two.txt two
HEAD2="$(localmain)"
PRE_MAIN="$(remote_main)"

passed; out="$(CHECK_STATUS=red queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -ne 0 ] && ok "4: to-forge refuses on a red publish" || bad "4: to-forge refuses on a red publish" "got rc=$rc out=$out"
want "4: names the red refusal" "red" "$out"
is "4: repo-map row is untouched (still queue.local)" "queue.local local/main" "$(landmode "$REPONAME")"
is "4: production is untouched" "$PRE_MAIN" "$(remote_main)"
is "4: local/main still exists at its post-land tip" "$HEAD2" "$(localmain)"
[ -f "$QDIR/$REPONAME/publish" ] && ok "4: the publish record is left open for the normal recovery" \
    || bad "4: the publish record is left open for the normal recovery" "no record found"

# Prove the refusal really changed nothing, not merely that it reported failure: settle the
# SAME publish green through the ordinary path, then confirm a retried transition succeeds.
CHECK_STATUS=green verdict >/dev/null
passed; out="$(CHECK_STATUS=green queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "4: after settling green, the retried transition succeeds" \
    || bad "4: after settling green, the retried transition succeeds" "got rc=$rc out=$out"
is "4: production now carries the previously-red round" "$HEAD2" "$(remote_main)"
queue to-local "$REPONAME" >/dev/null

# ============================================================================
echo
echo "5 — a diverged forge (refs differ) refuses the transition, changing nothing"
# ============================================================================
clear_calls
seed sp-tr3
land sp-tr3 three.txt three
PRE_MAIN="$(remote_main)"
PRE_LOCAL="$(localmain)"

CLONE="$TMP/clone"
timeout 5 git clone -q "$REMOTE" "$CLONE"
git -C "$CLONE" commit -q --allow-empty -m "foreign: not from local/main"
timeout 5 git -C "$CLONE" push -q origin main
FOREIGN="$(git -C "$CLONE" rev-parse main)"

passed; out="$(queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -ne 0 ] && ok "5: to-forge refuses when the forge diverged" || bad "5: to-forge refuses when the forge diverged" "got rc=$rc out=$out"
[ "$(callcount pr-create)" -eq 0 ] && ok "5: no PR was opened" || bad "5: no PR was opened" "$(cat "$CALL_LOG")"
is "5: repo-map row is untouched (still queue.local)" "queue.local local/main" "$(landmode "$REPONAME")"
is "5: production is unchanged (still the foreign commit)" "$FOREIGN" "$(remote_main)"
is "5: local/main is unchanged" "$PRE_LOCAL" "$(localmain)"

# Undo the foreign write so the next case sees a sane forge again.
git -C "$REMOTE" update-ref refs/heads/main "$PRE_MAIN" >/dev/null 2>&1

# ============================================================================
echo
echo "6 — the reverse flip syncs to the forge's CURRENT tip, not a stale one"
# ============================================================================
passed; out="$(CHECK_STATUS=green queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "6 setup: forward flip succeeds" || bad "6 setup: forward flip succeeds" "got rc=$rc out=$out"
FORWARD_TIP="$(remote_main)"

# Advance the forge further, directly, as ordinary queue.forge work would.
timeout 5 git clone -q "$REMOTE" "$TMP/clone-2"
git -C "$TMP/clone-2" commit -q --allow-empty -m "forge-only: landed while in queue.forge mode"
timeout 5 git -C "$TMP/clone-2" push -q origin main
ADVANCED="$(git -C "$TMP/clone-2" rev-parse main)"
[ "$ADVANCED" != "$FORWARD_TIP" ] && ok "6 setup: the forge actually advanced" \
    || bad "6 setup: the forge actually advanced" "did not move"

out="$(queue to-local "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "6: reverse flip succeeds" || bad "6: reverse flip succeeds" "got rc=$rc out=$out"
is "6: repo_land is back to queue.local" "queue.local local/main" "$(landmode "$REPONAME")"
is "6: the new local/main is synced to the forge's CURRENT tip, not the stale one" \
    "$ADVANCED" "$(localmain)"

# ============================================================================
echo
echo "7 — a bead IN_DELIVERY on spira-lc refuses the transition; once LANDED it proceeds"
# ============================================================================
lcfix_seed sp-trbusy IN_DELIVERY "$(localmain)"
passed; out="$(queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -ne 0 ] && ok "7: to-forge refuses with work in delivery" || bad "7: to-forge refuses with work in delivery" "got rc=$rc out=$out"
want "7: names the bead in delivery" "sp-trbusy" "$out"
is "7: repo-map row is untouched (still queue.local)" "queue.local local/main" "$(landmode "$REPONAME")"
lcfix_seed sp-trbusy LANDED "$(localmain)"
passed; out="$(queue to-forge "$REPONAME")"; rc=$?
[ "$rc" -eq 0 ] && ok "7: the same transition proceeds once the bead is LANDED" \
    || bad "7: the same transition proceeds once the bead is LANDED" "got rc=$rc out=$out"

tl_summary
