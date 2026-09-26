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
#   6. DELIVERS — a closed bead carrying any delivers: label survives without a landstate
#      record: its close never runs through the commit pipeline this invariant polices.
#   7. HAND-CLOSED — a closed bead with no $SPIRA_RUN/<id>.log (closed outside the pipeline
#      this harness worked) is never examined, landstate or not.
#   8. UNMAPPED REPO — a closed bead whose repo: label names a repo absent from the
#      repo-map is skipped, and the skip is logged once per repo per pass.
#   9. CANNOT RESOLVE REF — a mapped repo whose land ref cannot be determined is left
#      unjudged, not read as unlanded.
#  10. PARTITION ENUMERATION — a second persona's partition is examined too, not only the
#      first; and a pass with no persona declaring a partition says so.
#  11. CONTENT-LANDED — a closed bead labeled content-landed survives without a landstate
#      record: the Sending reaps it by merge-tree equivalence, not a naming commit, and
#      writes the label but no landstate/$id (sending.sh).
#
# None of these reopen the bead — the closed-work-bead close path now runs only through
# bead_close_on_land (lib.sh), so a violation here is a landing-pass bug, not a verdict to
# retry. CHECK 5 reports it to Ops and leaves the bead exactly as it found it.
#
# defect: sp-qsona
# tier: T3
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

# OTHER — a mapped repo whose land ref can never be determined: no remote (so rung 3 never
# runs a network-shaped remote query) and no commit (so rung 4's HEAD is unborn and
# rev-parse -verify fails). Both rungs return fast, purely from the ref store.
OTHER="$TMP/other"
git init -q -b main "$OTHER"

printf '%s | %s | pr | main | |\n' "$HOME_REPO" "$REPO" > "$TMP/repo-map"
printf '%s | %s | pr | | |\n' other "$OTHER" >> "$TMP/repo-map"

sentinel() {
    local faiths="${1:-t}"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$HOME_REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="$faiths" SPIRA_INFERENCE_EVERY=999999 \
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
echo "CONTENT-LANDED — a closed bead labeled content-landed survives without a landstate record:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-cl","title":"content landed","status":"closed","issue_type":"task","labels":["spira","plan","content-landed","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-cl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-cl.log"
rm -f "$RUN/landstate/sp-cl"
: > "$INC_LOG"

sentinel >/dev/null
is "sp-cl stays closed" closed "$(status_of sp-cl)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident for the content-landed bead" "sp-cl" "$inc_out"

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

# ======================================================================================
echo
echo "DELIVERS — a closed bead with a delivers: label survives without a landstate record:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-dlv","title":"delivered by note","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO","delivers:note:$RUN/sp-dlv.log"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-dlv","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-dlv.log"
rm -f "$RUN/landstate/sp-dlv"
: > "$INC_LOG"

sentinel >/dev/null
is "sp-dlv stays closed" closed "$(status_of sp-dlv)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident for a delivers:-labeled close" "sp-dlv" "$inc_out"

# ======================================================================================
echo
echo "HAND-CLOSED — a closed bead with no \$SPIRA_RUN/<id>.log is never examined:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-hand","title":"closed by hand","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-hand","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
rm -f "$RUN/sp-hand.log" "$RUN/landstate/sp-hand"
: > "$INC_LOG"

sentinel >/dev/null
is "sp-hand stays closed" closed "$(status_of sp-hand)"
inc_out="$(cat "$INC_LOG")"
nowant "no incident for a bead this harness never worked" "sp-hand" "$inc_out"

# ======================================================================================
echo
echo "UNMAPPED REPO — a closed bead naming a repo absent from the repo-map is skipped:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-ghost","title":"ghost repo","status":"closed","issue_type":"task","labels":["spira","plan","repo:ghost"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-ghost","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ghost.log"
rm -f "$RUN/landstate/sp-ghost"
: > "$INC_LOG"

out="$(sentinel)"
is "sp-ghost stays closed" closed "$(status_of sp-ghost)"
want "the skip is logged, naming the repo" "repo:ghost is not in repo-map" "$out"
inc_out="$(cat "$INC_LOG")"
nowant "no incident for an unmapped repo" "sp-ghost" "$inc_out"

# ======================================================================================
echo
echo "CANNOT RESOLVE REF — a mapped repo whose land ref cannot be determined is unjudged:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-other","title":"other repo, no resolvable ref","status":"closed","issue_type":"task","labels":["spira","plan","repo:other"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-other","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-other.log"
rm -f "$RUN/landstate/sp-other"
: > "$INC_LOG"

out="$(sentinel)"
is "sp-other stays closed" closed "$(status_of sp-other)"
want "CHECK 5 declines to judge, not reopen" "cannot resolve the ref" "$out"
inc_out="$(cat "$INC_LOG")"
nowant "no incident when the base cannot be resolved" "sp-other" "$inc_out"

# ======================================================================================
echo
echo "PARTITION ENUMERATION — a second persona's partition is examined too:"
# ======================================================================================
printf 'FAYTH_LABELS="ops"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/ops.fayth"
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["ops"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-opsbare","title":"bare closed, ops partition","status":"closed","issue_type":"task","labels":["ops","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-opsbare","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-opsbare.log"
rm -f "$RUN/landstate/sp-opsbare"
: > "$INC_LOG"

sentinel "t ops" >/dev/null
is "sp-opsbare stays closed" closed "$(status_of sp-opsbare)"
inc_out="$(cat "$INC_LOG")"
want "a partition other than the first persona's is examined too" "sp-opsbare" "$inc_out"
rm -f "$SH/chamber/ops.fayth"

# ======================================================================================
echo
echo "NO PARTITION DECLARED — a pass with no persona's fayth carrying labels says so:"
# ======================================================================================
out="$(sentinel ghost-persona)"
want "the pass logs that no partition is being checked" \
    "no persona in the chamber declares a partition" "$out"

# ======================================================================================
echo
echo "LANDED PROVEN BY THE COMMIT GRAPH — no landstate file, but the base names the bead:"
# ======================================================================================
# THE PRODUCTION FLOOD. The landing pass prunes landstate records after a landing, so every
# historical closed work bead has NO file at all; reading absence as "not landed" filed 138
# incidents in 30 minutes and pushed the pass past its TimeoutStartSec. The base's history
# is the other proof, by the same two subject shapes landed() trusts (lib.sh): the queue's
# own "spira: land <id>" merge subject, or an aeon's "<id>: ..." commit.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-qland","title":"queue-landed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-qland","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-cland","title":"colon-landed","status":"closed","issue_type":"bug","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-cland","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-qland.log" "$RUN/sp-cland.log"
rm -f "$RUN/landstate/sp-qland" "$RUN/landstate/sp-cland"
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-qland"
git -C "$REPO" commit -q --allow-empty -m "sp-cland: the fix"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
: > "$INC_LOG"

out="$(sentinel)"
inc_out="$(cat "$INC_LOG")"
nowant "(a) a 'spira: land <id>' subject on the base files no incident" "sp-qland" "$inc_out"
nowant "(b) an '<id>: ...' subject on the base files no incident"       "sp-cland" "$inc_out"
is "(a) sp-qland stays closed" closed "$(status_of sp-qland)"
is "(b) sp-cland stays closed" closed "$(status_of sp-cland)"

# ======================================================================================
echo
echo "GENUINELY UNLANDED — a mention or a longer id sharing the prefix is not a landing:"
# ======================================================================================
# The base carries commits that MENTION sp-unl and one for sp-unl.1 (a sibling whose id
# begins with it), but none is a landing record for sp-unl itself, so it is still reported.
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-unl","title":"unlanded","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-unl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-unl.log"
rm -f "$RUN/landstate/sp-unl"
git -C "$REPO" commit -q --allow-empty -m "follow-up for sp-unl: notes only"
git -C "$REPO" commit -q --allow-empty -m "sp-unl.1: a sibling's work"
git -C "$REPO" commit -q --allow-empty -m "spira: land sp-unl.1"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
: > "$INC_LOG"

out="$(sentinel)"
inc_out="$(cat "$INC_LOG")"
want "(c) a genuinely unlanded closed bead still files an incident" "REF=closed-not-landed:sp-unl " "$inc_out"

# ======================================================================================
echo
echo "A FLOOD IS IMPOSSIBLE — N+1 unlanded beads file exactly N, and the skip is logged:"
# ======================================================================================
testdb_reset
{
    printf '{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-04T00:00:00Z"}\n'
    for n in 1 2 3; do
        printf '{"id":"sp-fl%s","title":"flood","status":"closed","issue_type":"task","labels":["spira","plan","repo:%s"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-fl%s","depends_on_id":"sp-goal","type":"parent-child"}]}\n' "$n" "$HOME_REPO" "$n"
        touch "$RUN/sp-fl$n.log"; rm -f "$RUN/landstate/sp-fl$n"
    done
} | testdb_seed
: > "$INC_LOG"

out="$(SPIRA_CHECK5_MAX_FILE=2 sentinel)"
is   "(d) exactly SPIRA_CHECK5_MAX_FILE incidents are filed" 2 "$(grep -c 'REF=closed-not-landed:' "$INC_LOG")"
want "(d) the pass logs how many the cap skipped" "1 more not filed" "$out"

tl_summary
