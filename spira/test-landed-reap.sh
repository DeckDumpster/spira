#!/usr/bin/env bash
#
# test-landed-reap.sh — bead_close_on_land reaps a landed member's branch and worktree at
#   the moment it closes it, using the same verified deletion sending.sh uses
#   (spira_reap_landed_branch, lib.sh) — never a tip comparison, and it refuses to delete a
#   branch whose content is not actually on the repository's base.
#
#   ./test-landed-reap.sh
#
# WHY THIS EXISTS (sp-jci6o). bead_close_on_land is called from every LANDED land_mark site
# — queue.sh, verdict.sh, batch.sh, landing.sh — and used to do nothing but flip the bead's
# status. The per-pass Sending then had to re-walk every branch in every repository to
# rediscover, by ancestry, which of them had just landed, which is the cost this bead
# removes for queue-mode repositories. This suite proves the close itself now also reaps:
#
#   1. POSITIVE: a submitted bead whose repo:/branch: labels name a branch that IS an
#      ancestor of the base loses both its branch and its worktree.
#   2. THE REFUSAL (the safety this exists for): a submitted bead whose branch carries a
#      commit NOT on the base is still closed — that fact is independent of the branch —
#      but spira_destroy_branch's content fence refuses to delete it, exactly as it would
#      for any other caller that did not pre-verify safety. The worktree is freed anyway
#      (same shape as sending.sh's "KEEP superseded-unsafe": the branch keeps every commit,
#      so nothing is lost by freeing the checkout of it).
#   3. A submitted bead with no repo:/branch: labels closes cleanly with no branch to reap.
#
# defect: sp-jci6o
# tier: T1
# covers: spira/lib.sh spira/sending.sh queue/src/* spira/batch.sh
# hermetic-ok: a stub bd (a JSON-file-per-id fixture) and local git repos — no database, no
#   systemd, no real network
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
export SPIRA_CONF="$TMP/no-such-conf"

# --------------------------------------------------------------------------------------
# THE STUB BD — show, close and label, the only three seams bead_close_on_land and
# spira_reap_landed_branch reach. One JSON file per id under $STUB_BEADS_DIR, mutated in
# place by close/label so the assertions below read back what actually happened rather
# than a canned answer (the same contract test-sending.sh's stub proves).
# --------------------------------------------------------------------------------------
STUB_BEADS_DIR="$TMP/beads"; mkdir -p "$STUB_BEADS_DIR"
STUB_BD="$TMP/bd-stub"
cat > "$STUB_BD" <<'EOF'
#!/usr/bin/env bash
dir="${STUB_BEADS_DIR:?}"
cmd=""; skip_next=0; argv=()
for a in "$@"; do
    if [ "$skip_next" = 1 ]; then skip_next=0; continue; fi
    case "$a" in
        -C)                skip_next=1 ;;
        --json)            ;;
        --reason-file)     skip_next=1 ;;
        show|close|label)  cmd="$a" ;;
        *)                 argv+=("$a") ;;
    esac
done
case "$cmd" in
    show)
        f="$dir/${argv[0]:-}.json"
        if [ -f "$f" ]; then printf '['; cat "$f"; printf ']\n'; else printf '[]\n'; fi
        ;;
    close)
        f="$dir/${argv[0]:-}.json"
        [ -f "$f" ] || exit 0
        python3 - "$f" <<'PY'
import json, sys
f = sys.argv[1]
d = json.load(open(f))
d["status"] = "closed"
json.dump(d, open(f, "w"))
PY
        ;;
    label)
        f="$dir/${argv[1]:-}.json"
        [ -f "$f" ] || exit 0
        python3 - "$f" "${argv[0]:-}" "${argv[2]:-}" <<'PY'
import json, sys
f, sub, lbl = sys.argv[1], sys.argv[2], sys.argv[3]
d = json.load(open(f))
labs = d.get("labels") or []
if sub == "add" and lbl not in labs: labs.append(lbl)
if sub == "remove" and lbl in labs: labs.remove(lbl)
d["labels"] = labs
json.dump(d, open(f, "w"))
PY
        ;;
esac
exit 0
EOF
chmod +x "$STUB_BD"
export SPIRA_BD="$STUB_BD" STUB_BEADS_DIR SPIRA_DB="$TMP/no-such-db"

bead() {   # bead <id> <repo-label> <branch-label>
    local id="$1" repo="$2" br="$3"
    printf '{"id":"%s","status":"open","labels":["spira-submitted","repo:%s","branch:%s"],"dependencies":[]}' \
        "$id" "$repo" "$br" > "$STUB_BEADS_DIR/$id.json"
}
status_of() { python3 -c '
import json, sys
try: print(json.load(open(sys.argv[1])).get("status") or "")
except Exception: print("")' "$STUB_BEADS_DIR/$1.json" 2>/dev/null; }

# --------------------------------------------------------------------------------------
# THE GIT FIXTURE — one repository, one bare remote, one repo-map row.
# --------------------------------------------------------------------------------------
REPO="$TMP/repo"; REMOTE="$TMP/remote.git"; RUN="$TMP/run"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate"
export SPIRA_RUN="$RUN" SPIRA_REAPLOG="$RUN/reap.log"

REPO_NAME="$(basename "$REPO")"
printf '%s | %s | queue | | |\n' "$REPO_NAME" "$REPO" > "$TMP/repo-map"
export SPIRA_REPO_MAP="$TMP/repo-map" SPIRA_HOME="$HERE" SPIRA_HOME_REPO="$REPO_NAME" \
    SPIRA_REPO="$REPO"

# shellcheck disable=SC1090
. "$HERE/lib.sh"

branch_exists() { git -C "$REPO" show-ref --verify -q "refs/heads/$1" 2>/dev/null; }

echo "test-landed-reap.sh"

# ======================================================================================
echo
echo "1. POSITIVE: an ancestor branch is reaped at the close that lands it:"
# ======================================================================================
git -C "$REPO" branch spira/sp-good main
git -C "$REPO" worktree add -q "$RUN/worktree/sp-good" spira/sp-good
bead sp-good "$REPO_NAME" spira/sp-good

bead_close_on_land sp-good deadbeef

is   "sp-good is closed"                "closed" "$(status_of sp-good)"
is   "sp-good's branch is gone"         1 "$(branch_exists spira/sp-good; echo $?)"
is   "sp-good's worktree directory is gone" 1 "$([ -e "$RUN/worktree/sp-good" ]; echo $?)"

# ======================================================================================
echo
echo "2. THE REFUSAL: a branch carrying content not on the base is closed but NOT deleted:"
# ======================================================================================
git -C "$REPO" checkout -q -b spira/sp-bad main
printf 'unlanded work\n' > "$REPO/unlanded.txt"
git -C "$REPO" add unlanded.txt
git -C "$REPO" commit -q -m "sp-bad: work never merged to main"
git -C "$REPO" checkout -q main
git -C "$REPO" worktree add -q "$RUN/worktree/sp-bad" spira/sp-bad
bead sp-bad "$REPO_NAME" spira/sp-bad

bead_close_on_land sp-bad deadbeef

is   "sp-bad is closed anyway (closing is independent of the reap)" "closed" "$(status_of sp-bad)"
is   "sp-bad's branch SURVIVES — content fence refused the delete" 0 \
    "$(branch_exists spira/sp-bad; echo $?)"
is   "sp-bad's worktree is freed anyway (no commit is lost — the branch keeps them all)" 1 \
    "$([ -e "$RUN/worktree/sp-bad" ]; echo $?)"

# ======================================================================================
echo
echo "3. NO repo:/branch: labels: closes cleanly, nothing to reap, no crash:"
# ======================================================================================
printf '{"id":"sp-nolabel","status":"open","labels":["spira-submitted"],"dependencies":[]}' \
    > "$STUB_BEADS_DIR/sp-nolabel.json"

bead_close_on_land sp-nolabel deadbeef
rc=$?

is "sp-nolabel is closed" "closed" "$(status_of sp-nolabel)"
is "bead_close_on_land exits 0 with no branch to reap" 0 "$rc"

tl_summary
