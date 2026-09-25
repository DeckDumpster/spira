#!/usr/bin/env bash
#
# test-check5-delivers-check.sh — CHECK 5 accepts a close when a bead declares
#   delivers:check:<command> and the command exits 0; reopens when it exits non-zero.
#
# WHY THIS EXISTS. Infrastructure beads — those that clone a checkout, write config,
# install units — produce no commit, no child bead, and no file the bead authored. The
# existing delivers: types (beads, note, report) cover none of those cases, so such a
# bead loops forever: it does the machine work on every pass, finds nothing to commit,
# and is requeued. delivers:check:<command> fills the gap: the filer states the command
# that proves the machine state is in place, and CHECK 5 runs it on every sentinel pass.
# A zero exit means done; non-zero means not yet.
#
# THE COMMAND IS THE ACCEPTANCE CRITERION, WRITTEN AT FILING. An aeon cannot choose the
# check at close time, because then it grades its own homework by picking a check it has
# already satisfied. The filer writes it; the sentinel verifies it on every pass.
#
# CASES, each asserted (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a bare closed bead with no commit IS reopened. Ensures CHECK 5
#      is running and that the new case does not break the common path.
#
#   2. delivers:check:<passing command> — NOT reopened; command exits 0, state is present.
#
#   3. delivers:check:<failing command> — IS reopened; command exits non-zero, not yet.
#
#   4. delivers:check (no command) — IS reopened; malformed label is not evidence.
#
# defect: sp-hivwr
# covers: spira/sentinel.sh spira/aeon.sh
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
testdb_require test-check5-delivers-check
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5chk || { echo "test-check5-delivers-check: could not build a fixture database"; exit 1; }

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

# A command that always exits 0 (true), and one that always exits 1 (false).
CMD_PASS="true"
CMD_FAIL="false"

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

echo "test-check5-delivers-check.sh"

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
echo "delivers:check:<passing command> — NOT reopened; command exits 0:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-chk-pass","title":"check passes","status":"closed","issue_type":"task","labels":["spira","plan","delivers:check:$CMD_PASS","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-chk-pass","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-chk-pass.log"
is "sp-chk-pass starts closed" closed "$(status_of sp-chk-pass)"
out="$(sentinel)"
is "sp-chk-pass stays closed (command exits 0)" closed "$(status_of sp-chk-pass)"
nowant "the pass does not reopen it" "reopened sp-chk-pass" "$out"
want   "the pass records the verification" "delivers" "$out"

# ======================================================================================
echo
echo "delivers:check:<failing command> — IS reopened; command exits non-zero:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-chk-fail","title":"check fails","status":"closed","issue_type":"task","labels":["spira","plan","delivers:check:$CMD_FAIL","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-chk-fail","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-chk-fail.log"
is "sp-chk-fail starts closed" closed "$(status_of sp-chk-fail)"
out="$(sentinel)"
is "sp-chk-fail is reopened (command exits 1)" open "$(status_of sp-chk-fail)"
want "the pass says so" "reopened sp-chk-fail" "$out"
want "the reason names the check type" "delivers:check" "$out"

# ======================================================================================
echo
echo "delivers:check (no command) — IS reopened; malformed label:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-chk-bare","title":"check no cmd","status":"closed","issue_type":"task","labels":["spira","plan","delivers:check","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-chk-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-chk-bare.log"
is "sp-chk-bare starts closed" closed "$(status_of sp-chk-bare)"
out="$(sentinel)"
is "sp-chk-bare is reopened (no command)" open "$(status_of sp-chk-bare)"
want "the pass says so" "reopened sp-chk-bare" "$out"
want "the reason says no command" "has no command" "$out"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
