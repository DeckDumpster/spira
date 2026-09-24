#!/usr/bin/env bash
#
# test-check5-landed-no-delivers.sh — a closed bead whose commit IS on the base and which
#   carries NO delivers: label must be left closed.
#
# WHY THIS EXISTS. This is the case CHECK 5 sees most often — the ordinary bead, worked and
# landed by an aeon, with no delivers: label at all — and on 2026-09-09 it was the case that
# broke, in the direction that destroys finished work.
#
# THE DEFECT. The bulk listing is emitted as delimited columns and read with `read -r`. Bash
# treats TAB as IFS WHITESPACE, so a run of tabs collapses to a single delimiter and an EMPTY
# MIDDLE COLUMN vanishes, shifting every later column left by one. `delivers` is empty on
# almost every bead, so `read` handed it the NEXT column — started_at's ISO timestamp. A
# timestamp is not a recognised delivers type, so CHECK 5 took the delivers branch for beads
# that had declared nothing, charged an attempt and reopened them. Eleven beads were poisoned
# in ninety seconds, every one of them with commits on the base naming it. Demonstrated:
#
#   printf 'a\tb\t\tc\n' | while IFS=$'\t' read -r w x y z; do echo "[$y]"; done   ->  [c]
#
# THE FIX is \x1f as the separator at all three sites — emitter, sort and read — because it is
# not IFS whitespace and empty columns survive it. A placeholder for the empty value would
# have been a convention someone must remember; this makes the shape unconstructible.
#
# WHY THE EXISTING SUITE DID NOT CATCH IT. test-check5-nopayload.sh passes 24/0 against the
# broken code. Every one of its closed beads is EXPECTED to be reopened, or carries a
# delivers: label, so the collapsed column changed only the reason, never the verdict. The
# case that distinguishes them — landed, no delivers, must stay closed — was the one nobody
# wrote. A test that asserts an outcome reached by the wrong route is not covering the route.
#
# defect: sp-4z3s
# covers: spira/sentinel.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-landed-no-delivers
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up check5landed || { echo "test-check5-landed-no-delivers: could not build a fixture database"; exit 1; }

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
echo "test-check5-landed-no-delivers.sh"

# ======================================================================================
echo
echo "the ordinary bead — closed, commit on the base, no delivers: label — stays closed:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-land","title":"landed work","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-land","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-land.log"
git -C "$REPO" commit -q --allow-empty -m "fix: sp-land — the work"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
is "sp-land starts closed" closed "$(status_of sp-land)"
out="$(sentinel)"
# THE ASSERTION THE BUG BREAKS. Under the collapsed-column defect delivers became the
# started_at timestamp, the delivers branch fired, and this bead was reopened and charged.
is "sp-land is STILL closed — a landed bead is not reopened" closed "$(status_of sp-land)"
if [[ "$out" == *"reopened sp-land"* ]]; then
    bad "the pass did not reopen it" "$(printf '%s' "$out" | grep -o 'reopened sp-land[^\n]*' | head -1)"
else
    ok "the pass did not reopen it"
fi
if [[ "$out" == *"delivers"*"sp-land"* || "$out" == *"sp-land"*"delivers not verified"* ]]; then
    bad "it was not judged by the delivers branch" "delivers branch fired on a bead with no delivers: label"
else
    ok "it was not judged by the delivers branch"
fi
labels_after="$(B label list sp-land 2>&1)"
if [[ "$labels_after" == *"sp-attempt"* ]]; then
    bad "no attempt was charged" "wanted no sp-attempt-* label, got [$labels_after]"
else
    ok "no attempt was charged"
fi

# ======================================================================================
echo
echo "positive control — the same bead with NO commit IS still reopened:"
# ======================================================================================
# Without this, a CHECK 5 that had simply stopped running would pass the case above.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-nocommit","title":"never landed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-nocommit","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-nocommit.log"
out2="$(sentinel)"
is "sp-nocommit is reopened" open "$(status_of sp-nocommit)"
want "and for the RIGHT reason" "closed without landing" "$out2"

# ======================================================================================
echo
echo "the seam directly — an empty middle column survives the reader:"
# ======================================================================================
# The unit-level statement of the same fact, so a future change of separator is caught here
# rather than only through the two end-to-end cases above.
line="$(printf 'sp-x\x1frepo\x1f0\x1f0\x1f0\x1f\x1f2026-09-08T12:00:00Z')"
got_delivers=""; got_started=""
while IFS=$'\x1f' read -r _a _b _c _d _e f g; do got_delivers="$f"; got_started="$g"; done <<< "$line"
is "an empty delivers column reads as empty" "" "$got_delivers"
is "and started_at keeps its own value"      "2026-09-08T12:00:00Z" "$got_started"

echo
printf 'test-check5-landed-no-delivers.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
