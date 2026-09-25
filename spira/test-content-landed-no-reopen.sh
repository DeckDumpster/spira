#!/usr/bin/env bash
#
# test-content-landed-no-reopen.sh — CHECK 5 does not reopen a bead whose branch the
#   Sending reaped by content, nor a poisoned one; ordinary closed beads still are.
#
#   ./test-content-landed-no-reopen.sh
#
# WHY THIS EXISTS. sending.sh deletes a branch when content_landed says merging it would
# produce exactly the base tree — the work is on the base, but under someone else's commit,
# so no merge commit ever names the bead. One pass later CHECK 5 searches the base subjects
# for the id, finds nothing, and cannot fall back on its "the work is still on a branch"
# guard either, because the Sending deleted that branch. So it reopened the bead, a fresh
# aeon redid finished work, closed it, the Sending reaped it again, and CHECK 5 reopened it
# again. Measured on 2026-09-08: 99 reopens over 80 beads, 34 of them landing 86-143s after
# that same bead's own SENT — one sentinel pass, no jitter. sp-637b went six rounds, sp-62ji
# burned three aeons, and sp-0092 cost $7.97 for a single one of them.
#
# Poison did not bound it: sp-637b was poisoned after 3 attempts at 21:08:30 and reopened as
# attempt 4 at 21:12:54. So the poison label is terminal for this check too.
#
# THREE CASES, because an exemption with no positive control is indistinguishable from a
# check that never fires at all (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a closed bead with no commit naming it and no label IS reopened.
#   2. CONTENT-LANDED   — a closed bead carrying `content-landed` is NOT reopened.
#   3. POISONED         — a closed bead carrying `spira-poison` is NOT reopened.
#
# And the seam: the labels must actually be present in the BULK listing CHECK 5 reads them
# from. The supersession exemption once shipped broken because the field was absent there,
# and the first attempt at this fix shipped a guard that grepped `bd show` output for a JSON
# shape bd does not emit — it matched nothing, so the loop went on running while the bead
# said it was fixed.
#
# defect: sp-796o
# covers: spira/sentinel.sh spira/sending.sh
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
testdb_require test-content-landed-no-reopen
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up contentlanded || { echo "test-content-landed-no-reopen: could not build a fixture database"; exit 1; }

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
try:
    d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
    print(d[0].get("status") or "")
except Exception:
    pass'; }

echo "test-content-landed-no-reopen.sh"

seed() {
    testdb_reset || { echo "seed: testdb_reset failed" >&2; exit 1; }
    # A silent import failure produces wrong state — status_of returns empty and
    # assertions fail as "wanted [closed] got []" rather than "seed failed". Exit
    # loudly here so a broken bd or fixture is immediately visible (sp-4nk93).
    testdb_seed <<JSONL || { echo "seed: testdb_seed (bd import) failed" >&2; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-08T00:00:00Z"}
{"id":"sp-bare","title":"bare closed — no commit, no exemption","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-08T00:00:00Z","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-cont","title":"reaped by content","status":"closed","issue_type":"task","labels":["spira","plan","content-landed","repo:$HOME_REPO"],"updated_at":"2026-09-08T00:00:00Z","dependencies":[{"issue_id":"sp-cont","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-pois","title":"poisoned","status":"closed","issue_type":"task","labels":["spira","plan","spira-poison","repo:$HOME_REPO"],"updated_at":"2026-09-08T00:00:00Z","dependencies":[{"issue_id":"sp-pois","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
    # CHECK 5 only examines beads an aeon worked, evidenced by a session log.
    touch "$RUN/sp-bare.log" "$RUN/sp-cont.log" "$RUN/sp-pois.log"
}

# ======================================================================================
echo
echo "positive control — an ordinary closed bead with no commit IS reopened:"
# ======================================================================================
seed
is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare is reopened" open "$(status_of sp-bare)"
want "the pass says so" "reopened sp-bare" "$out"

# ======================================================================================
echo
echo "content-landed — a branch the Sending reaped by content is NOT reopened:"
# ======================================================================================
seed
is "sp-cont starts closed" closed "$(status_of sp-cont)"
out="$(sentinel)"
is "sp-cont stays closed" closed "$(status_of sp-cont)"
nowant "the pass does not reopen it" "reopened sp-cont" "$out"

# ======================================================================================
echo
echo "poison is terminal — a poisoned bead is NOT reopened:"
# ======================================================================================
seed
is "sp-pois starts closed" closed "$(status_of sp-pois)"
out="$(sentinel)"
is "sp-pois stays closed" closed "$(status_of sp-pois)"
nowant "the pass does not reopen it" "reopened sp-pois" "$out"

# ======================================================================================
# THE SEAM. The exemption reads its labels off the BULK listing, not off `bd show`. The
# first attempt at this fix grepped `bd show` output for '"label".*"content-landed"' — a
# shape bd never emits, since labels come back as a plain array under "labels" — so the
# guard matched nothing and the loop ran on while the bead reported itself fixed.
# ======================================================================================
echo
echo "the bulk listing carries both labels:"
seed
bulk="$(B list --status closed --limit 0 --label spira,plan --json 2>/dev/null)"
seen="$(python3 -c '
import json, sys
d = json.loads(sys.argv[1]); d = d if isinstance(d, list) else [d]
want = {"sp-cont": "content-landed", "sp-pois": "spira-poison"}
got = [k for k, v in want.items()
       if any(i["id"] == k and v in (i.get("labels") or []) for i in d)]
print(",".join(sorted(got)))
' "$bulk" 2>/dev/null)"
is "both labels are readable where CHECK 5 reads them" "sp-cont,sp-pois" "$seen"

echo
printf '%s: %d passed, %d failed\n' "$(basename "$0")" "$pass" "$fail"
[ "$fail" -eq 0 ]
