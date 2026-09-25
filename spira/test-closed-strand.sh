#!/usr/bin/env bash
#
# test-closed-strand.sh — stranded-bead detection and recovery (sp-bf31a).
#
# THREE SCENARIOS:
#
#   1. EJECTED-NOT-REQUEUED. A bead is closed with a branch and a landstate of EJECTED
#      (batch gate failure). The sentinel (CHECK6) reopens it on the next pass.
#      Positive control: an open bead with an EJECTED landstate is left alone.
#
#   2. LANDSTATE PRUNE. A closed bead's landstate file exists but the branch is gone.
#      The landing pass prunes the orphaned file.
#      Positive control: a closed bead WITH a live branch keeps its landstate.
#
#   3. ANOMALY SPLIT. SP_UNLANDED_N is split into SP_STRANDED_N (older than cert window)
#      and SP_CERT_N (within cert window). A bead closed 5 minutes ago counts as
#      awaiting cert, not stranded.
#
# covers: spira/landing.sh spira/lib.sh spira/cockpit.sh cockpit/health.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-closed-strand
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up strand || { echo "test-closed-strand: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main
mkdir -p "$RUN/worktree" "$RUN/landstate" "$SH/chamber"

cp "$HERE/landing.sh" "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/conf.sh" \
   "$HERE/incident.sh" "$HERE/skew.sh" "$HERE/sending.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub reflect.sh    'exit 0'
stub gate.sh       'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'
stub confine.sh    'exit 0'
stub gh            'exit 1'
stub ask.sh        'true'
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,spira-ask,spira-ci"\nFAYTH_MAX_CONCURRENT=0\n' \
    > "$SH/chamber/t.fayth"

printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$REPONAME" "$REPO" push main '' '' > "$SH/repo-map"

B()        { bd -C "$SPIRA_DB" "$@"; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }

printf '#!/usr/bin/env bash\nprintf %%s\\\\n inactive\n' > "$TMP/systemctl"; chmod +x "$TMP/systemctl"
printf '#!/usr/bin/env bash\nexit 0\n' > "$TMP/launch"; chmod +x "$TMP/launch"

sentinel() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="t" SPIRA_INFERENCE_EVERY=999999 \
    SPIRA_NOTIFY="$SH/ask.sh" SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_CONF="$TMP/no.conf" \
        bash "$SH/sentinel.sh" 2>&1
}

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_HOME_REPO="$REPONAME" \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    SPIRA_CONF="$TMP/no.conf" \
        bash "$SH/landing.sh" 2>&1
}

PAST="2026-09-01T00:00:00Z"

echo "test-closed-strand.sh"

# ======================================================================================
echo
echo "SCENARIO 1: EJECTED-NOT-REQUEUED — CHECK6 reopens a closed+EJECTED bead:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-ej","title":"ejected closed","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-ej","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-open","title":"open with ejected landstate","status":"open","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"$PAST","dependencies":[{"issue_id":"sp-open","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ej.log" "$RUN/sp-open.log"

# Create branches for both beads.
git -C "$REPO" worktree add -q -b "spira/sp-ej" "$RUN/worktree/sp-ej" main
git -C "$RUN/worktree/sp-ej" commit -q --allow-empty -m "sp-ej: work"
git -C "$REPO" worktree add -q -b "spira/sp-open" "$RUN/worktree/sp-open" main
git -C "$RUN/worktree/sp-open" commit -q --allow-empty -m "sp-open: work"

# Write EJECTED landstate for both beads.
_tip_ej="$(git -C "$REPO" rev-parse "spira/sp-ej")"
_tip_open="$(git -C "$REPO" rev-parse "spira/sp-open")"
printf 'EJECTED %s 0\n' "$_tip_ej"   > "$RUN/landstate/sp-ej"
printf 'EJECTED %s 0\n' "$_tip_open" > "$RUN/landstate/sp-open"

is "sp-ej starts closed"  closed "$(status_of sp-ej)"
is "sp-open starts open"  open   "$(status_of sp-open)"

out="$(landing)"
printf '%s\n' "$out" | grep -E "sp-ej|sp-open|ejected|EJECTED|reopened" | head -20 >&2

want   "CHECK6 reopens the closed+EJECTED bead"       "reopened sp-ej"    "$out"
is     "sp-ej is now open"                            open "$(status_of sp-ej)"
nowant "CHECK6 does not reopen the open+EJECTED bead" "reopened sp-open"  "$out"
is     "sp-open stays open"                           open "$(status_of sp-open)"

git -C "$REPO" worktree remove --force "$RUN/worktree/sp-ej"   2>/dev/null || true
git -C "$REPO" worktree remove --force "$RUN/worktree/sp-open" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-ej" "spira/sp-open" 2>/dev/null || true
rm -f "$RUN/landstate/sp-ej" "$RUN/landstate/sp-open"

# ======================================================================================
echo
echo "SCENARIO 2: LANDSTATE PRUNE — orphaned landstate file removed each pass:"
# ======================================================================================
testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-pruned","title":"no branch","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-pruned","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-kept","title":"has branch","status":"closed","issue_type":"task","labels":["spira","plan","repo:$REPONAME"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-kept","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-pruned.log" "$RUN/sp-kept.log"

# sp-pruned: has a landstate but no branch — the orphaned case.
printf 'RED none 0\n' > "$RUN/landstate/sp-pruned"

# sp-kept: has a landstate AND a live branch — must not be pruned.
git -C "$REPO" worktree add -q -b "spira/sp-kept" "$RUN/worktree/sp-kept" main
git -C "$RUN/worktree/sp-kept" commit -q --allow-empty -m "sp-kept: work"
printf 'RED none 0\n' > "$RUN/landstate/sp-kept"

[ -f "$RUN/landstate/sp-pruned" ] \
    && ok "sp-pruned landstate exists before the pass" \
    || bad "sp-pruned landstate exists before the pass" "file not created"

out="$(landing)"
printf '%s\n' "$out" | grep -i "pruning\|landstate" | head -10 >&2

if [ -f "$RUN/landstate/sp-pruned" ]; then
    bad "orphaned landstate sp-pruned was pruned" "file still exists after landing pass"
else
    ok "orphaned landstate sp-pruned was pruned by the landing pass"
fi
if [ -f "$RUN/landstate/sp-kept" ]; then
    ok "landstate sp-kept was NOT pruned (bead has a live branch)"
else
    bad "landstate sp-kept was NOT pruned" "file was incorrectly removed"
fi

git -C "$REPO" worktree remove --force "$RUN/worktree/sp-kept" 2>/dev/null || true
git -C "$REPO" branch -D "spira/sp-kept" 2>/dev/null || true
rm -f "$RUN/landstate/sp-kept"

# ======================================================================================
echo
echo "SCENARIO 3: ANOMALY SPLIT — SP_STRANDED_N vs SP_AWAITING_N:"
# ======================================================================================
# This scenario invokes cockpit.sh directly (like test-cockpit-unlanded.sh).
REAL_BD="$(PATH="$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin" command -v bd 2>/dev/null)"
if [ -z "$REAL_BD" ]; then
    echo "SKIP strand-anomaly-split: no bd binary" >&2
else
    BASE_PATH="$PATH"
    BD_PATH="${SPIRA_PATH:-}"
    TESTDB_BD_PATH="$(command -v bd)"

    ALPHA="$TMP/alpha"
    git init -q -b main "$ALPHA"
    git -C "$ALPHA" commit --allow-empty -m "init" -q
    git -C "$ALPHA" checkout -q -b spira/sp-old
    git -C "$ALPHA" commit --allow-empty -m "sp-old work" -q
    git -C "$ALPHA" checkout -q main
    git -C "$ALPHA" checkout -q -b spira/sp-new
    git -C "$ALPHA" commit --allow-empty -m "sp-new work" -q
    git -C "$ALPHA" checkout -q main

    MAP="$TMP/cmap"
    printf 'alpha | %s | push | main | |\n' "$ALPHA" > "$MAP"
    RUN2="$TMP/run2"; mkdir -p "$RUN2"
    SCOPE=alpha

    # sp-old: closed 200 minutes ago (beyond the 90-min cert window → stranded)
    # sp-new: closed 5 minutes ago (within cert window → awaiting)
    AGO200="$(date -u -d '200 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
              || date -u -v-200M +%Y-%m-%dT%H:%M:%SZ)"
    AGO5="$(date -u -d '5 minutes ago' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
            || date -u -v-5M +%Y-%m-%dT%H:%M:%SZ)"

    testdb_reset
    testdb_seed <<JSONL2
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["$SCOPE"],"updated_at":"$PAST"}
{"id":"sp-old","title":"stranded","status":"closed","priority":1,"closed_at":"$AGO200","labels":["$SCOPE","plan","repo:alpha"]}
{"id":"sp-new","title":"awaiting","status":"closed","priority":1,"closed_at":"$AGO5","labels":["$SCOPE","plan","repo:alpha"]}
JSONL2
    printf '{"type":"system","subtype":"init"}\n' > "$RUN2/sp-old.log"
    printf '{"type":"system","subtype":"init"}\n' > "$RUN2/sp-new.log"

    cout="$(env -i PATH="$BASE_PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_HOME="$HERE" \
        SPIRA_REPO="$ALPHA" SPIRA_HOME_REPO=alpha SPIRA_SCOPE_LABEL="$SCOPE" \
        SPIRA_RUN="$RUN2" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${TESTDB_BD_PATH:-$REAL_BD}" \
        SPIRA_REPO_MAP="$MAP" SPIRA_GOAL=sp-goal SPIRA_FAYTHS=t \
        SPIRA_PATH="$BD_PATH" SPIRA_CERT_WINDOW_MINS=90 \
        bash "$HERE/cockpit.sh" once 2>/dev/null)"

    cval() { printf '%s' "$cout" | grep "^$1=" | head -1 | sed "s/^$1=//"; }

    is "SP_UNLANDED_N is 2 (both have branch, no landstate)" "2" "$(cval SP_UNLANDED_N)"
    is "SP_STRANDED_N is 1 (sp-old closed 200 min ago)"      "1" "$(cval SP_STRANDED_N)"
    is "SP_CERT_N is 1 (sp-new closed 5 min ago)"            "1" "$(cval SP_CERT_N)"
fi

echo
printf 'test-closed-strand: %d ok, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
