#!/usr/bin/env bash
#
# test-check5-invariant.sh — CHECK 5 is one invariant: a closed work bead with no LANDED
#   landstate record (tip an ancestor of the base) is reported to Ops, not reopened, and
#   everything the invariant is not supposed to touch is left alone.
#
#   ./test-check5-invariant.sh
#
# THE CASES, each a pair with the positive control (law-absence-needs-a-positive-control):
#
#   1. POSITIVE CONTROL — a closed work bead with no landstate record files an Ops
#      incident naming it. Without this, every case below is indistinguishable from
#      CHECK 5 not running at all.
#   2. LANDED — a closed work bead whose landstate is LANDED, with a tip verified by
#      independent git ancestry (not the same text search CHECK 5 used to run) to be an
#      ancestor of the base, is NOT reported.
#   3. DROPPED — spira-dropped survives: the operator's verdict that no branch is coming.
#   4. SUPERSEDED — a superseded closed bead survives: its work lands under the
#      successor's name.
#   5. NON-CODE TYPE — a closed bead outside SPIRA_WORK_CLOSE_TYPES (issue_type=event) is
#      never examined, landstate or not — its close does not run through the
#      submitted/landed pipeline this invariant polices.
#
# None of these reopen the bead — the closed-work-bead close path now runs only through
# bead_close_on_land (lib.sh), so a violation here is a landing-pass bug, not a verdict to
# retry. CHECK 5 reports it to Ops and leaves the bead exactly as it found it.
#
# defect: sp-qsona
# covers: spira/sentinel.sh spira/lib.sh
# timeout: 120
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-check5-invariant
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up check5inv || { echo "test-check5-invariant: could not build a fixture database"; exit 1; }

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

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub governor.sh   'exit 0'
stub reflect.sh    'exit 0'
stub ask.sh        'true'

printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/t.fayth"

B() { bd -C "$SPIRA_DB" "$@"; }
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"
printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"

# THE MOCK CAPTURES EVERY CALL — a real incident.sh would write its own bead, which is
# incident.sh's own suite's job to verify; what this suite verifies is whether CHECK 5
# calls it, and with what, for each case.
INC_LOG="$TMP/incident.log"
: > "$INC_LOG"
cat > "$TMP/mock-incident.sh" <<MOCK
#!/usr/bin/env bash
printf 'REF=%s CAUSE=%s file %s\n' "\${SPIRA_INCIDENT_REF:-}" "\${SPIRA_INCIDENT_CAUSE:-}" "\$*" >> "$INC_LOG"
MOCK
chmod +x "$TMP/mock-incident.sh"

HOME_REPO="$(basename "$REPO")"
printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$HOME_REPO" "$REPO" pr main '' '' > "$TMP/repo-map"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$TMP/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no-such-conf" \
    SPIRA_INCIDENT_SH="$TMP/mock-incident.sh" \
    SPIRA_SKIP_RECLAIM=1 \
        bash "$SH/sentinel.sh" 2>&1
}

status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

echo "test-check5-invariant.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — a closed work bead with no landstate is reported to Ops:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-bare","title":"bare closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-bare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-bare.log"
rm -f "$RUN/landstate/sp-bare"
: > "$INC_LOG"

is "sp-bare starts closed" closed "$(status_of sp-bare)"
out="$(sentinel)"
is "sp-bare stays closed — CHECK 5 does not reopen" closed "$(status_of sp-bare)"
want "the pass reports it, not reopens it" "filing an Ops incident" "$out"
nowant "and never says reopened" "reopened sp-bare" "$out"
inc_out="$(cat "$INC_LOG")"
want "an incident is filed with the right ref" "REF=closed-not-landed:sp-bare" "$inc_out"
want "the incident names the bead" "sp-bare" "$inc_out"

# ======================================================================================
echo
echo "LANDED — ancestry-verified commit and landstate; bead is not reported:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-land","title":"land","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-land","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-land.log"

git -C "$REPO" commit -q --allow-empty -m "sp-land: implement the work"
land_sha="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

if git -C "$REPO" merge-base --is-ancestor "$land_sha" origin/main 2>/dev/null; then
    ok "landing confirmed by ancestry (--is-ancestor), independent of CHECK 5"
else
    bad "landing confirmed" "commit $land_sha is NOT an ancestor of origin/main — fixture is wrong"
fi
printf 'LANDED %s %s' "$land_sha" "$(date +%s)" > "$RUN/landstate/sp-land"
: > "$INC_LOG"

out="$(sentinel)"
is "sp-land stays closed" closed "$(status_of sp-land)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident is filed for a landed bead" "sp-land" "$inc_out"

# ======================================================================================
echo
echo "DROPPED and SUPERSEDED survive without a landstate record, same as before:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-drop","title":"dropped","status":"closed","issue_type":"task","labels":["spira","plan","spira-dropped","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-drop","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-succ","title":"successor","status":"open","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-succ","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-supr","title":"superseded","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-supr","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-supr","depends_on_id":"sp-succ","type":"supersedes"}]}
JSONL
touch "$RUN/sp-drop.log" "$RUN/sp-supr.log"
rm -f "$RUN/landstate/sp-drop" "$RUN/landstate/sp-supr"
: > "$INC_LOG"

sentinel >/dev/null
is "sp-drop stays closed" closed "$(status_of sp-drop)"
is "sp-supr stays closed" closed "$(status_of sp-supr)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident for the dropped bead" "sp-drop" "$inc_out"
nowant "no incident for the superseded bead" "sp-supr" "$inc_out"

# ======================================================================================
echo
echo "NON-CODE TYPE — a closed event bead with no landstate is never examined:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-ev","title":"an event bead","status":"closed","issue_type":"event","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-ev","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ev.log"
rm -f "$RUN/landstate/sp-ev"
: > "$INC_LOG"

sentinel >/dev/null
is "sp-ev stays closed" closed "$(status_of sp-ev)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident for a non-code type, landstate or not" "sp-ev" "$inc_out"

tl_summary
