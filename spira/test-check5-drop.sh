#!/usr/bin/env bash
#
# test-check5-drop.sh — CHECK 5's exemptions actually exempt, and its reopen still fires.
#
#   ./test-check5-drop.sh
#
# THE THREE CASES, each a pair (law-absence-needs-a-positive-control):
#
#   1. A closed bead with `spira-dropped` is NOT reopened — it was dropped by the operator.
#   2. A closed bead that has been superseded is NOT reopened — its work lands under the
#      successor's name.
#   3. An ordinary closed bead with no commit naming it IS reopened — that is the whole point
#      of CHECK 5 and without this control the exemptions are indistinguishable from a check
#      that never fires at all.
#
# WHY THIS SUITE EXISTS. The supersession exemption shipped broken and never once fired:
# `bd list` returns {"type"} where `bd show` returns {"dependency_type"}, so `sup` was 0
# for every bead and sp-dvlq was reopened every two minutes while carrying the dependency.
# The drop exemption (spira-dropped, 3828725) reads a label off the same bulk query and has
# the same shape of risk: an untested exemption on a data path that has already been wrong
# once.
#
# A REAL bd ON A FIXTURE DATABASE (law-prefer-the-real-dependency), because what is under
# test is how `bd list --status closed` represents labels and dependencies — precisely the
# seam that broke supersession.
#
# defect: sp-kufh
# covers: spira/sentinel.sh spira/lib.sh
# timeout: 120
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-drop
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5 || { echo "test-check5-drop: could not build a fixture database"; exit 1; }

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
stub governor.sh   'exit 0'
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

echo "test-check5-drop.sh"

# ======================================================================================
# SEED THE FIXTURE — three closed beads under the goal, each with a different exemption
# state, plus one log file per bead (CHECK 5 only examines beads that have a session log).
# ======================================================================================
seed() {
    testdb_reset
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-drop","title":"dropped","status":"closed","issue_type":"task","labels":["spira","plan","spira-dropped","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-drop","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-succ","title":"successor","status":"open","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-succ","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-supr","title":"superseded","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-supr","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-supr","depends_on_id":"sp-succ","type":"supersedes"}]}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
    # CHECK 5 only examines beads that have a session log.
    touch "$RUN/sp-drop.log" "$RUN/sp-supr.log" "$RUN/sp-bare.log"
}

labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))' 2>/dev/null; }

echo
echo "the positive control — an ordinary closed bead IS reopened:"
seed
is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare is reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"
# NO COUNTER LABEL IS WRITTEN (sp-lzt). The attempt is counted when the NEXT aeon
# claims the bead (in_progress transition), not when sentinel reopens it. The events
# trail records the claim; CHECK4 reads it on demand to apply the poison threshold.
nowant "and no sp-attempt-* counter label is written" "sp-attempt-" "$(labels_of sp-bare)"
# THE REOPEN NOTE NAMES THE REMEDY. A superseded close is the normal end of a duplicate,
# and the note is the only thing the next person reads — without the hint the loop is:
# close, watch it reopen, close again, disbelieve the database (sp-da6k).
note_text="$(B show sp-bare --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("notes") or "")' 2>/dev/null)"
want "and the reopen note names bd supersede as the remedy" "bd supersede" "$note_text"

echo
echo "a dropped bead survives CHECK 5:"
seed
is "sp-drop starts closed" closed "$(status_of sp-drop)"
out="$(sentinel)"
is "sp-drop is still closed" closed "$(status_of sp-drop)"
nowant "the pass does not say it was reopened" "reopened sp-drop" "$out"

echo
echo "a superseded bead survives CHECK 5:"
seed
is "sp-supr starts closed" closed "$(status_of sp-supr)"
out="$(sentinel)"
is "sp-supr is still closed" closed "$(status_of sp-supr)"
nowant "the pass does not say it was reopened" "reopened sp-supr" "$out"

# ======================================================================================
# THE DROP EXEMPTION READS THE LABEL FROM THE BULK QUERY, not from a per-bead `bd show`.
# This is the same shape of risk that broke supersession: if the label is absent from the
# bulk listing's JSON, the exemption is dead. Verify the field is present by asking the
# same query the sentinel issues, with python reading what python would read.
# ======================================================================================
echo
echo "the bulk listing carries both exemption signals:"

bulk="$(B list --status closed --limit 0 --label spira,plan --json 2>/dev/null)"
has_drop="$(python3 -c '
import json, sys
d = json.loads(sys.argv[1])
d = d if isinstance(d, list) else [d]
for i in d:
    if i["id"] == "sp-drop" and "spira-dropped" in (i.get("labels") or []):
        print("yes"); break
else:
    print("no")
' "$bulk" 2>/dev/null)"
is "sp-drop's spira-dropped label is in the bulk listing" "yes" "$has_drop"

has_sup="$(python3 -c '
import json, sys
d = json.loads(sys.argv[1])
d = d if isinstance(d, list) else [d]
for i in d:
    if i["id"] == "sp-supr":
        for dep in (i.get("dependencies") or []):
            t = dep.get("dependency_type") or dep.get("type")
            if t == "supersedes":
                print("yes"); break
        else:
            continue
        break
else:
    print("no")
' "$bulk" 2>/dev/null)"
is "sp-supr's supersedes dependency is in the bulk listing" "yes" "$has_sup"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
