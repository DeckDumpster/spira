#!/usr/bin/env bash
#
# test-check5-delivers-action.sh — CHECK 5 accepts a close when a bead declares
#   delivers:action; reopens when no delivers label is set (positive control).
#
# CASES, each asserted (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a bare closed bead with no commit IS reopened. Ensures CHECK 5
#      is running and the new case does not break the ordinary path.
#
#   2. delivers:action — NOT reopened; the close reason carries the evidence.
#
# defect: sp-2d14d
# covers: spira/sentinel.sh spira/incident.sh spira/watchtower.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
# timeout: 60
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
testdb_require test-check5-delivers-action
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5act || { echo "test-check5-delivers-action: could not build a fixture database"; exit 1; }

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

printf 'FAYTH_LABELS="spira,${SPIRA_PLAN_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

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

PAST="2026-09-01T00:00:00Z"

echo "test-check5-delivers-action.sh"

# ======================================================================================
echo
echo "positive control — a bare closed bead with no commit IS reopened:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-bare.log"
is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare is reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"

# ======================================================================================
echo
echo "delivers:action — NOT reopened; close reason carries the evidence:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-act","title":"action bead","status":"closed","issue_type":"task","labels":["spira","plan","delivers:action","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-act","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-act.log"
is "sp-act starts closed" closed "$(status_of sp-act)"
out="$(sentinel)"
is "sp-act stays closed (delivers:action accepted)" closed "$(status_of sp-act)"
nowant "the pass does not reopen it" "reopened sp-act" "$out"
want   "the pass records the verification" "delivers" "$out"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
