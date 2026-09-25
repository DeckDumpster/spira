#!/usr/bin/env bash
#
# test-check5-unmapped-repo.sh — CHECK 5 skips closed beads whose repo: label is absent
#   from the repo-map and logs the absent repo at most once per pass.
#
#   ./test-check5-unmapped-repo.sh
#
# THREE CASES (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a closed bead with a MAPPED repo and no commit IS reopened.
#      Without this, a "skip everything" implementation reads as correct.
#   2. UNMAPPED — a closed bead with an unmapped repo: label is NOT reopened.
#      An unmapped repo is out of this install's scope; deregistration is legitimate.
#   3. DEDUP — with two closed beads in the same unmapped repo, the absent-repo log
#      line appears exactly once, not once per bead.
#
# defect: sp-qxjjd
# covers: spira/sentinel.sh spira/lib.sh
# timeout: 120
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
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
testdb_require test-check5-unmapped-repo
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5 || { echo "test-check5-unmapped-repo: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'

printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

B() { bd -C "$SPIRA_DB" "$@"; }
cat > "$TMP/launch" <<'L'
#!/usr/bin/env bash
exit 0
L
cat > "$TMP/systemctl" <<'S'
#!/usr/bin/env bash
printf '%s\n' "inactive"
S
chmod +x "$TMP/launch" "$TMP/systemctl"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SKIP_RECLAIM=1 \
        bash "$SH/sentinel.sh" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

echo "test-check5-unmapped-repo.sh"

seed() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-bare","title":"mapped-bare","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-un1","title":"unmapped-one","status":"closed","issue_type":"task","labels":["spira","plan","repo:gone-repo"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-un1","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-un2","title":"unmapped-two","status":"closed","issue_type":"task","labels":["spira","plan","repo:gone-repo"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-un2","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
    touch "$RUN/sp-bare.log" "$RUN/sp-un1.log" "$RUN/sp-un2.log"
}

# ======================================================================================
# POSITIVE CONTROL — a mapped-repo bead with no commit is still reopened.
# ======================================================================================
echo
echo "positive control — mapped repo bead with no commit is reopened:"
seed
is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare is reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"

# ======================================================================================
# UNMAPPED REPO — closed beads for a repo not in the repo-map are not reopened.
# ======================================================================================
echo
echo "unmapped repo — closed bead is not reopened:"
seed
is "sp-un1 starts closed" closed "$(status_of sp-un1)"
out="$(sentinel)"
is "sp-un1 is still closed" closed "$(status_of sp-un1)"
nowant "the pass does not reopen it" "reopened sp-un1" "$out"

is "sp-un2 is still closed" closed "$(status_of sp-un2)"
nowant "the pass does not reopen sp-un2 either" "reopened sp-un2" "$out"

# ======================================================================================
# DEDUP — with two beads in the same unmapped repo, the log line appears exactly once.
# ======================================================================================
echo
echo "dedup — absent-repo log line appears once, not once per bead:"
seed
out="$(sentinel)"
count="$(printf '%s\n' "$out" | grep -c "repo:gone-repo" || true)"
is "absent-repo log line count is 1" "1" "$count"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
