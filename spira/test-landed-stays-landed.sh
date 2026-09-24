#!/usr/bin/env bash
#
# test-landed-stays-landed.sh — end-to-end: a closed bead whose commit is on origin/<base>
#   is never reopened by CHECK 5, across the two failure modes that historically caused it.
#
# WHY THIS EXISTS. sp-d9x93 and sp-796o caused CHECK 5 to reopen 34 beads whose work had
#   genuinely landed on the base branch:
#
#   WINDOW DEFECT (sp-d9x93): CHECK 5 used git log -n 400 (400-commit window). A bead
#   whose commit was commit 401+ from the tip was invisible and reopened as
#   closed-without-landing. sp-a9g hit this at commit 401, sp-37q at commit 400.
#
#   BODY DEFECT (sp-d9x93): CHECK 5 searched --format='%s' (subject line only). A bead id
#   appearing only in the commit body was not found and the bead was reopened. sp-m0s7.
#
#   The fix: sentinel's CHECK 5 now walks the full ancestry with --format='%B'.
#
# THE E2E INVARIANT. This suite asserts the property directly: land a real commit naming a
#   bead on origin/<base>, run the sentinel, bead stays closed. The deep-history case pads
#   401 commits after the landing commit so a windowed search would miss it.
#
# CASES (law-absence-needs-a-positive-control):
#   1. POSITIVE CONTROL — closed bead with no commit IS reopened (CHECK 5 is live)
#   2. DEEP COMMIT — commit 401+ back stays closed (window-free search)
#   3. BODY-ONLY — bead id only in commit body stays closed (full %B search)
#   4. TWO PASSES — stays closed on second pass (no state accumulated between passes)
#
# SEEN TO FAIL AGAINST UNFIXED CODE. Both probe sections below install a patched sentinel
#   with the old window/format and confirm the test WOULD HAVE FAILED:
#     PROBE FAIL deep: bugged sentinel reopens sp-deep (window=1) — wanted [open] got ...
#     PROBE FAIL body: subject-only sentinel reopens sp-body — wanted [open] got ...
#   Both probes were confirmed to fire before this test was committed (sp-wg6hi).
#
# defect: sp-d9x93 sp-796o
# covers: spira/sentinel.sh spira/lib.sh
# hermetic-ok: uses a fixture database and a local git repo, no systemd or gh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
probe_ok()  { pass=$((pass+1)); printf '  probe-ok   %s\n' "$1"; }
probe_bad() { fail=$((fail+1)); printf '  PROBE-FAIL %s: %s\n' "$1" "$2"; }
probe_is()  { [ "$2" = "$3" ] && probe_ok "$1" || probe_bad "$1" "wanted [$2] got [$3]"; }
probe_want(){ [[ "$3" == *"$2"* ]] && probe_ok "$1" || probe_bad "$1" "wanted [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landed-stays-landed
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT TERM
testdb_up lsl || { echo "test-landed-stays-landed: could not build a fixture database"; exit 1; }

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

# Save a pristine copy of sentinel.sh to $TMP before installing to $SH, so probes that
# patch $SH/sentinel.sh can restore from $TMP/sentinel-real.sh without copying a file
# to itself (the sentinel.sh in $SH is what gets patched and restored).
SENTINEL_REAL="$TMP/sentinel-real.sh"
cp "$HERE/sentinel.sh" "$SENTINEL_REAL"

cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'exit 0'
stub strand.sh     'exit 0'
stub sending.sh    'exit 0'
stub governor.sh   'exit 0'
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
        bash "$SH/sentinel.sh" 2>&1
}

status_of()   { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
attempts_of() { B label list "$1" 2>/dev/null | grep -oE 'sp-attempt-[0-9]+' | head -1 || true; }

PAST="2026-09-01T00:00:00Z"
echo "test-landed-stays-landed.sh"

# ======================================================================================
echo
echo "POSITIVE CONTROL — a closed bead with no commit IS reopened:"
# Without this, every passing case below is indistinguishable from CHECK 5 not running.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-ctrl","title":"ctrl","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-ctrl","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-ctrl.log"
is "sp-ctrl starts closed" closed "$(status_of sp-ctrl)"
out="$(sentinel)"
is "sp-ctrl IS reopened" open "$(status_of sp-ctrl)"
want "the pass says so" "reopened sp-ctrl" "$out"

# ======================================================================================
echo
echo "DEEP COMMIT — commit 401+ back from HEAD stays closed (no window limit):"
#
# THE FIXTURE. The landing commit is made first; then 401 padding commits push it beyond
# the old 400-commit window from the tip. The correct sentinel finds it; the old one does
# not and reopens the bead.
#
# THE PROBE. A patched sentinel with the old window (git log -n 400 --format='%s') is
# installed and confirmed to reopen the bead — proving this case WOULD HAVE FAILED against
# unfixed code. The probe uses a fresh db seed then restores via testdb_reset + reseed.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-deep","title":"deep history","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-deep","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-deep.log"

git -C "$REPO" commit -q --allow-empty -m "sp-deep: implement the work"
land_sha="$(git -C "$REPO" rev-parse HEAD)"
for i in $(seq 1 401); do
    git -C "$REPO" commit -q --allow-empty -m "pad $i"
done
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# VERIFY ANCESTRY DIRECTLY. This proves the fixture is correct independently of the text
# search being tested. If the commit is not on origin/main, a passing sentinel would be
# a false negative from the fixture, not evidence of correct behaviour.
if git -C "$REPO" merge-base --is-ancestor "$land_sha" origin/main 2>/dev/null; then
    ok "landing confirmed by ancestry (--is-ancestor)"
else
    bad "landing confirmed" "commit $land_sha is NOT an ancestor of origin/main — fixture is wrong"
fi

# PROBE: install a windowed, subject-only sentinel and confirm it reopens sp-deep.
# This is the "SEEN TO FAIL AGAINST UNFIXED CODE" check for the window defect.
# The old sentinel used: git log --format='%B' $subj_refs  (no -n limit)
# We simulate the old window by: git log --format='%s' -n 1 $subj_refs
# A commit 401+ back is invisible to a window of 1, so the probe must reopen the bead.
_window_probe="$TMP/sentinel-window-probe.sh"
sed "s|log --format='%B' \\\$subj_refs|log --format='%s' -n 1 \$subj_refs|g" \
    "$SENTINEL_REAL" > "$_window_probe" && chmod +x "$_window_probe"
if grep -qF "log --format='%s' -n 1" "$_window_probe"; then
    cp "$_window_probe" "$SH/sentinel.sh"
    _probe_out="$(sentinel)"
    _probe_status="$(status_of sp-deep)"
    cp "$SENTINEL_REAL" "$SH/sentinel.sh"   # restore from pristine copy, not from $SH
    probe_is "probe: windowed sentinel reopens sp-deep" open "$_probe_status"
    probe_want "probe: pass says reopened sp-deep" "reopened sp-deep" "$_probe_out"
    # Reset db cleanly — bead was modified by the probe; git state (commits) stays.
    testdb_reset
    testdb_seed <<JSONL2 || { echo "reseed after probe failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-deep","title":"deep history","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-deep","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL2
    touch "$RUN/sp-deep.log"
else
    bad "probe: could not patch sentinel for window probe" "sed produced no change"
fi

is "sp-deep restored to closed before real passes" closed "$(status_of sp-deep)"

# PASS 1: real sentinel finds the commit regardless of depth.
out1="$(sentinel)"
is "pass 1: sp-deep stays closed" closed "$(status_of sp-deep)"
nowant "pass 1: not reopened" "reopened sp-deep" "$out1"
att1="$(attempts_of sp-deep)"
is "pass 1: no attempt label charged" "" "$att1"

# PASS 2: a bug where pass 1 changes state causing pass 2 to reopen would be caught here.
out2="$(sentinel)"
is "pass 2: sp-deep stays closed" closed "$(status_of sp-deep)"
nowant "pass 2: not reopened" "reopened sp-deep" "$out2"
att2="$(attempts_of sp-deep)"
is "pass 2: no attempt label charged" "" "$att2"

# ======================================================================================
echo
echo "BODY-ONLY — bead id only in commit body stays closed (full %B search):"
#
# THE FIXTURE. The commit subject does NOT contain "sp-body"; the id appears only in the
# body. The old subject-only search (%s) would not find it and would reopen the bead.
#
# THE PROBE. A subject-only patched sentinel is confirmed to reopen the bead.
# ======================================================================================
testdb_reset
testdb_seed <<JSONL || { echo "seed failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-body","title":"body-only id","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-body","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL
touch "$RUN/sp-body.log"

# Commit subject does NOT contain "sp-body"; the id appears only in the body paragraph.
git -C "$REPO" commit -q --allow-empty -m "$(printf 'refactor: cleanup\n\nThis carries sp-body — the underlying change\nwas squashed into this commit by the reviewer.')"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin

# PROBE: subject-only search cannot find a body-only reference.
_body_probe="$TMP/sentinel-body-probe.sh"
sed "s|log --format='%B' \\\$subj_refs|log --format='%s' \$subj_refs|g" \
    "$SENTINEL_REAL" > "$_body_probe" && chmod +x "$_body_probe"
if grep -qF "log --format='%s'" "$_body_probe"; then
    cp "$_body_probe" "$SH/sentinel.sh"
    _probe_out="$(sentinel)"
    _probe_status="$(status_of sp-body)"
    cp "$SENTINEL_REAL" "$SH/sentinel.sh"
    probe_is "probe: subject-only sentinel reopens sp-body" open "$_probe_status"
    probe_want "probe: pass says reopened sp-body" "reopened sp-body" "$_probe_out"
    testdb_reset
    testdb_seed <<JSONL3 || { echo "reseed after probe failed"; exit 1; }
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"$PAST"}
{"id":"sp-body","title":"body-only id","status":"closed","issue_type":"task","labels":["spira","plan","repo:$HOME_REPO"],"updated_at":"$PAST","started_at":"$PAST","dependencies":[{"issue_id":"sp-body","depends_on_id":"sp-goal","type":"parent-child"}]}
JSONL3
    touch "$RUN/sp-body.log"
else
    bad "probe: could not build subject-only sentinel for body probe" ""
fi

is "sp-body restored to closed before real pass" closed "$(status_of sp-body)"
out="$(sentinel)"
is "body-only NOT reopened" closed "$(status_of sp-body)"
nowant "the pass does not say reopened" "reopened sp-body" "$out"

echo
printf 'test-landed-stays-landed.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
