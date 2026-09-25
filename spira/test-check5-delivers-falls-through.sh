#!/usr/bin/env bash
#
# test-check5-delivers-falls-through.sh — a failed delivers: check must fall through to the
#   commit-naming check, not reopen on its own.
#
# THE CONTRACT, as incident.sh states it where the label is written:
#
#   "When an Ops session commits code naming the bead, the commit-naming check accepts the
#    close and this label is never consulted. When no commit is made (the SOP already
#    existed and no file changed), the sentinel checks this label."
#
# That is an either/or: commit OR declared evidence. The bead is only reopened when NEITHER
# is present.
#
# THE DEFECT. CHECK 5's delivers branch `continue`s on BOTH outcomes — verified and
# unverified — so a bead carrying a delivers: label never reaches the commit-naming check
# below it. The either/or became delivers-only, and the commit path's tolerances went with
# it: that path leaves a bead closed when its branch is merely ahead of the base ("work
# exists; CHECK 6 lands it"), because landing is the Sending's job and takes a few minutes.
# The delivers branch has no such tolerance and reopens immediately.
#
# WHAT IT COST. sp-lh8r, 2026-09-15: an aeon fixed it, committed, and closed. The bead
# carried delivers:note:$SPIRA_RUN/sop/applied.jsonl — a path nothing had written, because
# the fix was code and not a runbook application. CHECK 5 reopened it 45 seconds after the
# close, three times, at $1.39 / $0.23 / $0.67. The work was correct every time and landed
# unchanged as f7ffc30. With a fleet ceiling of one aeon the bead owned the only slot for an
# hour, so nothing else in the graph could be worked at all.
#
# FOURTEEN open beads carried an unsatisfiable delivers:note: path when this was written, so
# the loop was not specific to that bead — it was waiting under every one of them.
#
# covers: spira/sentinel.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-delivers-falls-through
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5deliv || { echo "test-check5-delivers-falls-through: could not build a fixture database"; exit 1; }

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
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"
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
        bash "$SH/sentinel.sh" 2>&1
}
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

PAST="2026-09-01T00:00:00Z"
GONE="$TMP/never-written.jsonl"          # the unsatisfiable deliverable
echo "test-check5-delivers-falls-through.sh"

# ======================================================================================
echo
echo "commit on the base + unsatisfiable delivers: — the commit wins, bead stays closed:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-cmt","title":"committed work with a stale delivers label","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO","delivers:note:$GONE"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-cmt","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-cmt.log"
git -C "$REPO" commit -q --allow-empty -m "sp-cmt: the work"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
[ ! -e "$GONE" ] || { echo "fixture error: $GONE should not exist"; exit 1; }
is "sp-cmt starts closed" closed "$(status_of sp-cmt)"
out="$(sentinel)"
is "sp-cmt is STILL closed — the commit satisfies the close" closed "$(status_of sp-cmt)"
if [[ "$out" == *"reopened sp-cmt"* ]]; then
    bad "it was not reopened on the delivers label alone" \
        "$(printf '%s' "$out" | grep -o 'reopened sp-cmt.*' | head -1 | cut -c1-90)"
else
    ok "it was not reopened on the delivers label alone"
fi

# ======================================================================================
echo
echo "POSITIVE CONTROL — same unsatisfiable delivers:, NO commit — still reopened:"
# ======================================================================================
# Without this the fix above could be "never reopen on delivers", which would delete the
# check rather than sequence it. The declared-evidence path must still catch a bead that
# closed on nothing at all.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-none","title":"no commit and no evidence","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO","delivers:note:$GONE"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-none","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-none.log"
is "sp-none starts closed" closed "$(status_of sp-none)"
out="$(sentinel)"
is "sp-none IS reopened — closed on nothing" open "$(status_of sp-none)"

# ======================================================================================
echo
echo "delivers: that VERIFIES still accepts the close with no commit:"
# ======================================================================================
# The label's own purpose: an Ops session that applied a runbook and changed no code.
testdb_reset
PRESENT="$TMP/present.jsonl"
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-ev","title":"evidence written, no commit","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO","delivers:note:$PRESENT"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-ev","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ev.log"
printf '{"applied":1}\n' > "$PRESENT"      # mtime is now, i.e. after started_at
is "sp-ev starts closed" closed "$(status_of sp-ev)"
out="$(sentinel)"
is "sp-ev stays closed — its declared evidence is present" closed "$(status_of sp-ev)"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
