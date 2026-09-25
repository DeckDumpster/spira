#!/usr/bin/env bash
#
# test-check5-landstate.sh — CHECK 5 reads landstate before reopening a closed bead.
#
# THE DEFECT (sp-e0ugn). CHECK 5's only exemption was a local branch with commits ahead
# of the base. A bead in the queue (CERTIFIED, BATCHED) with its branch deleted by the
# reaper had neither — so CHECK 5 reopened work that the landing pass was already handling.
#
# THREE CASES (law-absence-needs-a-positive-control):
#
#   POSITIVE CONTROL — a closed bead with no commit and no landstate IS reopened.
#
#   BATCHED — a closed bead with landstate BATCHED and no local branch is NOT reopened.
#
#   CERTIFIED-RESTORE — a closed bead with landstate CERTIFIED, an existing tip object,
#     and no local branch: branch is restored, bead stays closed.
#
# SEEN RED WITHOUT THE FIX. Against the unfixed sentinel (before landstate check):
#   - BATCHED case: bead reopened → assertion "BATCHED: stays closed" fails as
#       FAIL BATCHED: stays closed: wanted [closed] got [open]
#   - CERTIFIED-RESTORE case: bead reopened instead of branch restored → assertion fails.
#
# defect: sp-e0ugn
# covers: spira/sentinel.sh spira/batch.sh spira/conf.sh
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
testdb_require test-check5-landstate
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5ls || { echo "test-check5-landstate: could not build a fixture database"; exit 1; }

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate" "$SH/chamber"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'
printf 'FAYTH_LABELS="spira,${SPIRA_PLAN_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH/chamber/t.fayth"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

B() { bd -C "$SPIRA_DB" "$@"; }
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_SKIP_RECLAIM=1 SPIRA_SKIP_CLOSED_CHECK=0 \
        bash "$SH/sentinel.sh" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

PAST="2026-09-01T00:00:00Z"
echo "test-check5-landstate.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — closed bead, no landstate, no commit IS reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-ctrl","title":"ctrl","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-ctrl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ctrl.log"
rm -f "$RUN/landstate/sp-ctrl"
is "sp-ctrl starts closed" closed "$(status_of sp-ctrl)"
out="$(sentinel)"
is "POSITIVE CONTROL: sp-ctrl IS reopened" open "$(status_of sp-ctrl)"
want "POSITIVE CONTROL: pass says reopened" "reopened sp-ctrl" "$out"

# ======================================================================================
echo
echo "BATCHED — closed bead, landstate BATCHED, no local branch: NOT reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-batched","title":"batched","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-batched","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-batched.log"
# No local branch for sp-batched.
git -C "$REPO" branch -D "spira/sp-batched" 2>/dev/null || true
printf 'BATCHED fakeshabrakefakeshabrakefakeshabrakefakeshab %s\n' "$(date +%s)" \
    > "$RUN/landstate/sp-batched"
is "BATCHED: starts closed" closed "$(status_of sp-batched)"
out="$(sentinel)"
is "BATCHED: stays closed" closed "$(status_of sp-batched)"
nowant "BATCHED: not reopened" "reopened sp-batched" "$out"
want "BATCHED: pipeline message logged" "landstate=BATCHED" "$out"

# ======================================================================================
echo
echo "CERTIFIED-RESTORE — closed bead, landstate CERTIFIED, tip exists, no local branch:"
echo "  branch is restored, bead stays closed:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-cert","title":"cert","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-cert","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-cert.log"
# Create a commit for sp-cert but delete the local branch.
git -C "$REPO" checkout -q -b spira/sp-cert main
printf 'cert\n' > "$REPO/sp-cert.txt"
git -C "$REPO" add sp-cert.txt
git -C "$REPO" commit -q -m "sp-cert: implement"
_cert_tip="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" checkout -q main
git -C "$REPO" branch -D spira/sp-cert
# Write CERTIFIED landstate with the real tip.
printf 'CERTIFIED %s %s\n' "$_cert_tip" "$(date +%s)" > "$RUN/landstate/sp-cert"
is "CERTIFIED-RESTORE: starts closed"         closed "$(status_of sp-cert)"
is "CERTIFIED-RESTORE: branch absent before"  "" "$(git -C "$REPO" show-ref --hash refs/heads/spira/sp-cert 2>/dev/null || true)"
out="$(sentinel)"
is "CERTIFIED-RESTORE: stays closed"          closed "$(status_of sp-cert)"
nowant "CERTIFIED-RESTORE: not reopened"      "reopened sp-cert" "$out"
want   "CERTIFIED-RESTORE: restore logged"    "branch restored" "$out"
is "CERTIFIED-RESTORE: branch restored"       "$_cert_tip" "$(git -C "$REPO" show-ref --hash refs/heads/spira/sp-cert 2>/dev/null || true)"

echo
printf 'test-check5-landstate.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
