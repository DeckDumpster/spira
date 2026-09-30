#!/usr/bin/env bash
#
# test-verdict-parity.sh — sp-flj4a parity proof (TEMPORARY: lives only on the commit that
# still carries spira/verdict.sh). Every scenario is built twice from the same fixed-date
# commits, so SHAs are identical; one copy is settled by `bash verdict.sh <repo>`, the other
# by `queue verdict <repo>`. Compared: exit status, output, the queue directory's files,
# landstate, landing.log, the forge calls, the mail and helper calls, and every ref of the
# repository and its remote. Intended differences (queue/DESIGN-verdict.md §4) are named
# per scenario and asserted as differences, not hidden.
#
# tier: T1
# covers: queue/src/* spira/verdict.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-verdict-parity
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up vparity || { echo "test-verdict-parity: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export GIT_AUTHOR_DATE='2026-09-30T00:00:00Z' GIT_COMMITTER_DATE='2026-09-30T00:00:00Z'
export SPIRA_GIT_NAME=t SPIRA_GIT_EMAIL=t@t

echo "test-verdict-parity.sh"

SH="$TMP/spira"; mkdir -p "$SH"
cp -r "$HERE"/*.sh "$HERE"/*.py "$SH/" 2>/dev/null || true
chmod +x "$SH"/*.sh 2>/dev/null || true
STUBS="$TMP/stubs"; mkdir -p "$STUBS"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$STUBS/$1"; chmod +x "$STUBS/$1"; }
# Helpers both implementations run by name: each logs its argv (and stdin) to $S/helpers.
stub mail.sh 'printf "mail.sh %s\n" "$*" >> "$S/helpers"; cat >> "$S/helpers"; echo >> "$S/helpers"'
stub attribute.sh 'printf "attribute.sh %s\n" "$*" >> "$S/helpers"; echo "ATTR test-a.sh owner=pa-1 method=single"'
stub testenv 'printf "testenv %s\n" "$*" >> "$S/helpers"'
stub batcher 'printf "batcher %s\n" "$*" >> "$S/helpers"; echo "id=pj-7"'
export PATH="$STUBS:$PATH"

cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; shift
printf '%s %s\n' "$cmd" "$*" >> "$S/forge-calls"
case "$cmd" in
    check-status) cat "$S/status" 2>/dev/null ;;
    run-id) cat "$S/run-id" 2>/dev/null ;;
    run-metadata) cat "$S/run-meta" 2>/dev/null ;;
    *) : ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

REPONAME=fixv
# build <dir> <mode> — a fresh scenario in <dir>: remote, checkout, run dir, repo-map.
build() {
    S="$1"; export S
    rm -rf "$S"; mkdir -p "$S"
    REMOTE="$S/remote.git"; REPO="$S/repo"; RUN="$S/run"; QDIR="$RUN/queue"
    git init -q --bare -b main "$REMOTE"
    git init -q -b main "$REPO"
    git -C "$REPO" commit -q --allow-empty -m base
    git -C "$REPO" remote add origin "$REMOTE"
    git -C "$REPO" push -q origin main
    mkdir -p "$RUN/worktree" "$QDIR/$REPONAME" "$RUN/landstate"
    : > "$S/forge-calls"; : > "$S/helpers"
    if [ "$2" = local ]; then
        git -C "$REPO" branch local/main main
        printf '%s | %s | queue.local | local/main | | |\n' "$REPONAME" "$REPO" > "$S/repo-map"
    else
        printf '%s | %s | queue | origin/main | | |\n' "$REPONAME" "$REPO" > "$S/repo-map"
    fi
}
# commit_on <ref> <file> <content> -> new sha on a new branch w-<file>
commit_on() {
    git -C "$REPO" checkout -q -B "w-$2" "$1"
    printf '%s\n' "$3" > "$REPO/$2"; git -C "$REPO" add "$2"; git -C "$REPO" commit -q -m "work $2"
    git -C "$REPO" checkout -q --detach
    git -C "$REPO" rev-parse "w-$2"
}

env_run() {
    env SPIRA_CONF=/nonexistent SPIRA_HOME="$SH" SPIRA_HOME_REPO="$REPONAME" SPIRA_REPO="$REPO" \
        SPIRA_RUN="$RUN" SPIRA_QUEUE_DIR="$QDIR" SPIRA_REPO_MAP="$S/repo-map" \
        SPIRA_FORGE="$SH/forge-fixture.sh" SPIRA_LIFECYCLE_ENFORCE=0 SPIRA_QUEUE_LOCK_WAIT=2 \
        LANDSTATE="$RUN/landstate" "$@"
}

# snapshot -> everything observable, normalised (epochs, the scenario dir, git push chatter).
snapshot() {
    local rc="$1" out="$2" fix=""
    [ -f "$QDIR/$REPONAME/publish-red" ] && fix="$(sed -n 's/^fix_forward=//p' "$QDIR/$REPONAME/publish-red")"
    {
        echo "== rc $rc"
        echo "== out"; printf '%s\n' "$out" | sed -E 's/run stuck \([0-9]+s, idle [0-9]+s\)/run stuck (<A>s, idle <I>s)/' | grep -vE '^Deleted branch |^To |^   [0-9a-f]+\.\.[0-9a-f]+ |^ \+ |^ - \[deleted\]|^ \* \[new branch\]'
        echo "== queue files"
        (cd "$QDIR" && find . -type f ! -name lock ! -name '*.lock' ! -name step.lock | sort | while read -r f; do echo "-- $f"; cat "$f"; done)
        echo "== landstate"
        (cd "$RUN/landstate" 2>/dev/null && find . -type f | sort | while read -r f; do echo "-- $f"; cat "$f"; done)
        echo "== landing.log"; cat "$RUN/landing.log" 2>/dev/null
        echo "== forge"; cat "$S/forge-calls"
        echo "== helpers"; cat "$S/helpers"
        echo "== refs"; git -C "$REPO" for-each-ref --format='%(refname) %(objectname)' refs/heads refs/remotes
        echo "== remote"; git -C "$REMOTE" for-each-ref --format='%(refname) %(objectname)'
    } | sed -e "s#$S#<S>#g" -e 's/\b1[0-9]\{9\}\b/<T>/g' ${fix:+-e "s/$fix/<FIX>/g"}
}

# parity <name> <mode> <setup-fn> [intended-diff-needle]
parity() {
    local name="$1" mode="$2" setup="$3" needle="${4:-}" old new orc nrc
    build "$TMP/old" "$mode"; "$setup"
    old="$(env_run bash "$SH/verdict.sh" "$REPONAME" 2>&1)"; orc=$?
    old="$(snapshot "$orc" "$old")"
    build "$TMP/new" "$mode"; "$setup"
    new="$(env_run queue verdict "$REPONAME" 2>&1)"; nrc=$?
    new="$(snapshot "$nrc" "$new")"
    if [ -z "$needle" ]; then
        if [ "$old" = "$new" ]; then
            ok "$name: identical verdict"
        else
            bad "$name: identical verdict" "$(diff <(printf '%s\n' "$old") <(printf '%s\n' "$new"))"
        fi
    else
        # An intended difference: prove it is there, and show it.
        if [ "$old" != "$new" ] && printf '%s' "$new" | grep -qF -- "$needle"; then
            ok "$name: intended difference ($needle)"
        else
            bad "$name: intended difference ($needle)" "$(diff <(printf '%s\n' "$old") <(printf '%s\n' "$new"))"
        fi
        echo "   --- diff old/new ($name) ---"; diff <(printf '%s\n' "$old") <(printf '%s\n' "$new") | sed 's/^/   /'
    fi
}

# ---------------------------------------------------------------- queue.local scenarios
pub_rec() {   # a publish PR covering one landed commit on local/main
    local t; t="$(commit_on local/main a.txt a)"
    git -C "$REPO" branch -f local/main "$t"
    git -C "$REPO" push -q origin "$t:refs/heads/spira/publish/P"
    printf 'pr=5\nhead=%s\nbase=%s\nmembers=pa-1:%s\nopened=100\nbranch=spira/publish/P\nremote=origin\nforge_branch=main\n' \
        "$t" "$(git -C "$REPO" rev-parse main)" "$t" > "$QDIR/$REPONAME/publish"
}
L_none()     { :; }
L_missing()  { printf 'pr=5\nhead=x\n' > "$QDIR/$REPONAME/publish"; }
L_green()    { pub_rec; printf 'green\nrun-url: http://r/1\n' > "$S/status"; }
L_moved()    { pub_rec; git -C "$REPO" push -q -f origin "$(commit_on main z.txt z):refs/heads/main"; printf 'green\n' > "$S/status"; }
L_pending()  { pub_rec; printf 'pending\n' > "$S/status"; }
L_fault()    { pub_rec; printf 'provision_fault\n' > "$S/status"; }
L_red()      { pub_rec; printf 'red\nred-suite: test-b.sh\nred-suite: test-a.sh\nrun-url: http://r/9\n' > "$S/status"; }
L_red_bare() { pub_rec; printf 'red\n' > "$S/status"; }

echo; echo "queue.local — the publish PR"
parity "L1 no record"            local L_none
parity "L2 record missing field" local L_missing "== rc 1"
parity "L3 green fast-forward"   local L_green
parity "L4 green, forge moved"   local L_moved "== rc 1"
parity "L5 pending"              local L_pending
parity "L6 provision_fault"      local L_fault
parity "L7 red, suites named"    local L_red
parity "L8 red, no suites"       local L_red_bare

# ---------------------------------------------------------------- queue.forge scenarios
B0=""
batch_rec() {   # batch_rec <owner> [extra] — two members merged onto origin/main as the head
    local ta tb head
    B0="$(git -C "$REPO" rev-parse origin/main)"
    ta="$(commit_on "$B0" a.txt a)"; tb="$(commit_on "$B0" b.txt b)"
    git -C "$REPO" checkout -q -B spira/queue/Q "$B0"
    git -C "$REPO" merge -q --no-ff -m "spira: land pa-1" "$ta"
    git -C "$REPO" merge -q --no-ff -m "spira: land pb-2" "$tb"
    git -C "$REPO" checkout -q --detach
    head="$(git -C "$REPO" rev-parse spira/queue/Q)"
    git -C "$REPO" push -q origin spira/queue/Q
    printf 'pr=12\nhead=%s\nbase=%s\nmembers=pa-1:%s pb-2:%s\nopened=100\nbranch=spira/queue/Q\nowner=%s\n%s' \
        "$head" "$B0" "$ta" "$tb" "$1" "${2:-}" > "$QDIR/$REPONAME/open"
    HEAD_Q="$head"
}
F_malformed() { printf 'pr=12\n' > "$QDIR/$REPONAME/open"; }
F_claimed()   { batch_rec concierge; printf 'green\n' > "$S/status"; }
F_young()     { batch_rec batcher; printf 'pending\n' > "$S/status"; }
F_stuck()     { batch_rec batcher; printf 'pending\n' > "$S/status"; echo 77 > "$S/run-id"; printf 'started-at: 100\nlast-activity: 150\n' > "$S/run-meta"; }
F_rerun()     { batch_rec batcher; printf 'harness_fault\n' > "$S/status"; echo 77 > "$S/run-id"; }
F_exhaust()   { batch_rec batcher $'retries=2\n'; printf 'harness_fault\n' > "$S/status"; }
F_mismatch()  { batch_rec batcher; printf 'green\nhead-sha: 0000\n' > "$S/status"; }
F_nohead()    { batch_rec batcher; printf 'green\n' > "$S/status"; }
F_green()     { batch_rec batcher; printf 'green\nhead-sha: %s\nflaky: test-f.sh\n' "$HEAD_Q" > "$S/status"
                git -C "$REPO" branch spira/queue/OLD "$B0"; }
F_moved()     { batch_rec batcher; printf 'green\nhead-sha: %s\n' "$HEAD_Q" > "$S/status"
                git -C "$REPO" push -q origin "$(commit_on "$B0" c.txt c):refs/heads/main"; git -C "$REPO" fetch -q origin; }
F_conflict()  { batch_rec batcher; printf 'green\nhead-sha: %s\n' "$HEAD_Q" > "$S/status"
                git -C "$REPO" push -q origin "$(commit_on "$B0" b.txt conflicting):refs/heads/main"; git -C "$REPO" fetch -q origin; }
F_inbase()    { batch_rec batcher; printf 'green\nhead-sha: %s\n' "$HEAD_Q" > "$S/status"
                git -C "$REPO" push -q origin "w-a.txt:refs/heads/main"; git -C "$REPO" fetch -q origin; }
F_red_batcher() { batch_rec batcher; printf 'red\nred-suite: test-a.sh\nrun-url: http://r/3\n' > "$S/status"; }
F_red_judged()  { batch_rec batcher $'judgement=pj-1\n'; printf 'red\nred-suite: test-a.sh\n' > "$S/status"; }
F_red_bare()    { batch_rec batcher; printf 'red\n' > "$S/status"; }
F_red_hand()    { batch_rec operator; printf 'red\nred-suite: test-a.sh\nrun-url: http://r/3\n' > "$S/status"; }
F_unknown()     { batch_rec batcher; printf 'weird\n' > "$S/status"; }

echo; echo "queue.forge — the open batch PR"
parity "F1 malformed record"            forge F_malformed "== rc 1"
parity "F2 concierge claim refused"     forge F_claimed
parity "F3 pending, young"              forge F_young
parity "F4 pending, stuck -> cancel"    forge F_stuck
parity "F5 harness fault -> rerun"      forge F_rerun
parity "F6 harness fault, exhausted"    forge F_exhaust
parity "F7 green, head mismatch"        forge F_mismatch
parity "F8 green, head missing"         forge F_nohead
parity "F9 green, fast-forward + reap"  forge F_green
parity "F10 green, base moved -> rebuild" forge F_moved
parity "F11 green, base moved, conflict" forge F_conflict
parity "F12 green, member in new base"  forge F_inbase
parity "F13 red, batcher-owned"         forge F_red_batcher "--home "
parity "F14 red, judgement recorded"    forge F_red_judged
parity "F15 red, no annotations"        forge F_red_bare
parity "F16 red, hand-cut (D1)"         forge F_red_hand "batcher judgement-ci"
parity "F17 unknown status"             forge F_unknown "== rc 1"

tl_summary
