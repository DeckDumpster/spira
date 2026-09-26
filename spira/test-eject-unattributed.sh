#!/usr/bin/env bash
#
# test-eject-unattributed.sh — an ejection note names every suite the PR run turned red
# that the ejected member could own, not only the ones verdict.sh reproduced for that
# member's diff selection (sp-pnbdz). Also verifies the per-bead eject-count counter that
# landing.sh reads to force a full-corpus gate after two consecutive ejections.
#
#   1. Positive control: single member, single red suite it reproduces — no unattributed
#      section (also the pair control for case 2: all run-red suites are reproduced).
#   2. Two members, three red suites: member A's diff selects and reproduces one; the
#      other two are unattributed — named in both the note and the landstate record.
#   3. A FAIL line and an unattributed suite coexist in the same note without either
#      dropping the other.
#   4. Each ejection of the same member increments $SPIRA_RUN/eject-count/<id>.
#
# The repro batch is stubbed via SPIRA_QUEUE_REPRO_BATCH; no container is used.
# The forge is a local fixture; no network is reached.
#
# covers: spira/verdict.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()    { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()   { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()    { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()  { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant(){ [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require eject-unattributed
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up eject-unattributed || { echo "test-eject-unattributed: could not build fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"
REMOTE="$TMP/remote.git"
RUN="$TMP/run"
SH="$TMP/spira"
REPONAME=fixture-repo
LANDSTATE="$RUN/landstate"
QUEUEDIR="$RUN/queue"

git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH" "$LANDSTATE" "$QUEUEDIR/$REPONAME"

cp "$HERE"/*.sh "$SH/"

cat > "$SH/repo-map" <<RMAP
$REPONAME | $REPO | queue | origin/main | | |
RMAP

FORGE_LOG="$TMP/forge-log"
FORGE_STATUS_FILE="$TMP/forge-status"
SUITES_LOG="$TMP/suites-log"
REPRO_FAIL_FILE="$TMP/repro-fail"
REPRO_FAULT_FILE="$TMP/repro-fault"
MAIL_LOG="$TMP/mail-log"
export FORGE_LOG FORGE_STATUS_FILE SUITES_LOG REPRO_FAIL_FILE REPRO_FAULT_FILE MAIL_LOG

printf 'red\n' > "$FORGE_STATUS_FILE"
: > "$FORGE_LOG"; : > "$SUITES_LOG"; : > "$REPRO_FAIL_FILE"; : > "$REPRO_FAULT_FILE"; : > "$MAIL_LOG"

cat > "$SH/forge-fixture.sh" <<'FORGE'
#!/usr/bin/env bash
cmd="${1:-}"; shift; repo="${1:-}"; shift
case "$cmd" in
    check-status) cat "${FORGE_STATUS_FILE}" 2>/dev/null || printf 'pending\n' ;;
    run-id)       printf 'run-99\n' ;;
    pr-close)     printf '%s\tclose\n' "${1:-}" >> "$FORGE_LOG" ;;
    workflow-rerun) printf '%s\trerun\n' "${1:-}" >> "$FORGE_LOG" ;;
    pr-create)
        n=$(( $(wc -l < "$FORGE_LOG" 2>/dev/null || echo 0) + 1 ))
        printf '%s\n' "$n"
        ;;
    *) printf 'forge-fixture: unknown: %s\n' "$cmd" >&2; exit 1 ;;
esac
FORGE
chmod +x "$SH/forge-fixture.sh"

# _repro_is_red merges the member's tip onto base with --no-ff before testing, so the
# ref this stub receives is a merge commit, not the branch tip itself. Identify the
# member by parent commit (established idiom, test-verdict.sh case 24) rather than by
# comparing branch names against a merge SHA, which never matches.
cat > "$SH/repro-stub.sh" <<'REPRO'
#!/usr/bin/env bash
while [[ "${1:-}" == --* ]]; do shift 2; done
ref="${1:-}"
parents="$(git -C "${SPIRA_REPO:-.}" log --no-walk --pretty="%P" "$ref" 2>/dev/null)"
fail_list="$(cat "${REPRO_FAIL_FILE}" 2>/dev/null || true)"
for f in $fail_list; do
    tip="$(git -C "${SPIRA_REPO:-.}" rev-parse "$f" 2>/dev/null || true)"
    [ -n "$tip" ] || continue
    if printf '%s' "$parents" | grep -qF "$tip"; then
        [ -n "${REPRO_FAIL_LINE:-}" ] && printf '%s\n' "$REPRO_FAIL_LINE"
        exit 1
    fi
done
exit 0
REPRO
chmod +x "$SH/repro-stub.sh"

cat > "$SH/suites.sh" <<'SUITES'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SUITES_LOG"
SUITES
chmod +x "$SH/suites.sh"

cat > "$SH/mail.sh" <<'MAIL'
#!/usr/bin/env bash
body="$(cat)"
[ -n "${MAIL_LOG:-}" ] && printf '%s\n' "$body" >> "$MAIL_LOG" || true
MAIL
chmod +x "$SH/mail.sh"

verdict() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_QUEUE_DIR="$QUEUEDIR" \
    SPIRA_QUEUE_CI_MAXSEC=3600 \
    SPIRA_QUEUE_INFRA_RETRIES=2 \
    SPIRA_FORGE="$SH/forge-fixture.sh" \
    SPIRA_QUEUE_REPRO_BATCH="$SH/repro-stub.sh" \
        bash "$SH/verdict.sh" "$@" 2>&1
}

notes_of() {
    "${TESTDB_BD:-bd}" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | sed -n '/^[[{]/,$p' \
        | python3 -c 'import sys,json; d=json.load(sys.stdin); print((d[0].get("notes","") or ""))' 2>/dev/null
}

batch_file()    { printf '%s/%s/open' "$QUEUEDIR" "$REPONAME"; }
land_state_of() { awk '{print $1}' "$LANDSTATE/${1:-}" 2>/dev/null; }

plant_bead() {
    printf \
        '{"id":"%s","title":"%s","status":"closed","issue_type":"task","labels":[],"updated_at":"2026-09-16T00:00:00Z","closed_at":"2026-09-16T00:00:00Z","dependencies":[]}\n' \
        "$1" "$1" | testdb_seed
}

# make_branch <id> <file> — create branch spira/<id> with one specific file.
make_branch() {
    local id="$1" file="$2"
    local bwt="$RUN/worktree/$id"
    rm -rf "$bwt"
    git -C "$REPO" worktree add -q -b "spira/$id" "$bwt" origin/main 2>/dev/null || true
    mkdir -p "$bwt/$(dirname "$file")"
    printf '%s\n' "$id" > "$bwt/$file"
    git -C "$bwt" add -A
    git -C "$bwt" commit -q -m "$id: work"
}

# build_batch <id1> <id2> ... — merge pre-created branches and write open batch record.
build_batch() {
    local base_sha; base_sha="$(git -C "$REPO" rev-parse origin/main)"
    local wt="$RUN/worktree/.batch-build"
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    git -C "$REPO" worktree add -q --detach "$wt" "$base_sha"
    local members=() id tip
    for id in "$@"; do
        tip="$(git -C "$REPO" rev-parse "spira/$id")"
        git -C "$wt" merge -q --no-edit --no-ff -m "spira: land $id" "$tip" >/dev/null 2>&1
        members+=("$id:$tip")
        printf 'BATCHED %s %s\n' "$tip" "$(date +%s)" > "$LANDSTATE/$id"
    done
    local batch_head; batch_head="$(git -C "$wt" rev-parse HEAD)"
    local batch_br="spira/queue/test-$$"
    git -C "$REPO" branch -f "$batch_br" "$batch_head" 2>/dev/null || true
    git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    {
        printf 'pr=42\n'
        printf 'head=%s\n' "$batch_head"
        printf 'base=%s\n' "$base_sha"
        printf 'members=%s\n' "${members[*]}"
        printf 'opened=%s\n' "$(date +%s)"
        printf 'branch=%s\n' "$batch_br"
    } > "$(batch_file)"
    printf '%s\n' "$batch_head"
}

clean_case() {
    rm -f "$(batch_file)"
    : > "$FORGE_LOG"; : > "$SUITES_LOG"; : > "$REPRO_FAIL_FILE"; : > "$REPRO_FAULT_FILE"; : > "$MAIL_LOG"
    printf 'red\nred-suite: test-canary.sh\n' > "$FORGE_STATUS_FILE"
    find "$LANDSTATE" -maxdepth 1 -type f -delete 2>/dev/null || true
    rm -rf "$RUN/eject-count" 2>/dev/null || true
    rm -rf "$QUEUEDIR/$REPONAME/unreproduced" 2>/dev/null || true
    rm -rf "$RUN/landing.log" 2>/dev/null || true
    local wt
    for wt in "$RUN/worktree"/*; do
        [ -d "$wt" ] || continue
        git -C "$REPO" worktree remove -f "$wt" 2>/dev/null || true
    done
    rm -rf "$RUN/worktree" && mkdir -p "$RUN/worktree"
    git -C "$REPO" worktree prune 2>/dev/null || true
    git -C "$REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' 2>/dev/null \
        | while read -r br; do git -C "$REPO" branch -D "$br" 2>/dev/null || true; done
    testdb_reset
}

echo "test-eject-unattributed.sh"

printf 'red\nred-suite: test-canary.sh\n' > "$FORGE_STATUS_FILE"

# =============================================================================
# 1. POSITIVE CONTROL / PAIR CONTROL (law-absence-needs-a-positive-control): a
#    single member reproduces the run's only red suite. Nothing is unattributed
#    — the pair to case 2, proving the "not attributed" text is conditional on
#    an actual gap rather than always printed.
# =============================================================================
testdb_reset
make_branch sp-eu-ctrl spira/canary.sh
build_batch sp-eu-ctrl > /dev/null
plant_bead sp-eu-ctrl
printf 'spira/sp-eu-ctrl\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null
is     "1. control: sp-eu-ctrl ejected"              "EJECTED" "$(land_state_of sp-eu-ctrl)"
nowant "1. control: no unattributed section in note"  "not attributed" "$(notes_of sp-eu-ctrl)"
is     "1. control: landstate has no 5th field"       "" \
       "$(awk '{print $5}' "$LANDSTATE/sp-eu-ctrl" 2>/dev/null)"
clean_case

# =============================================================================
# 2. UNATTRIBUTED SUITES NAMED SEPARATELY. Run turns three suites red; member A's
#    diff selects and reproduces only test-canary.sh. The other two are red on
#    the same run but never attributed to anyone — the note must name them
#    separately from the reproduced suite, and the landstate record must carry
#    both lists in distinct fields (sp-pnbdz: without this, a builder fixes only
#    the named suite and the branch bounces again on the unattributed rest).
# =============================================================================
testdb_reset
make_branch sp-eu-a spira/canary.sh
make_branch sp-eu-b sp-eu-b.txt
build_batch sp-eu-a sp-eu-b > /dev/null
for id in sp-eu-a sp-eu-b; do plant_bead "$id"; done
printf 'red\nred-suite: test-canary.sh\nred-suite: test-extra-1.sh\nred-suite: test-extra-2.sh\n' \
    > "$FORGE_STATUS_FILE"
printf 'spira/sp-eu-a\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null
is   "2. unattr: sp-eu-a ejected"                  "EJECTED" "$(land_state_of sp-eu-a)"
want "2. unattr: note names reproduced suite"      "test-canary.sh" "$(notes_of sp-eu-a)"
want "2. unattr: note lists unattributed suites"   "not attributed" "$(notes_of sp-eu-a)"
want "2. unattr: note has test-extra-1"            "test-extra-1.sh" "$(notes_of sp-eu-a)"
is   "2. unattr: landstate reproduced field"       "test-canary.sh" \
     "$(awk '{print $4}' "$LANDSTATE/sp-eu-a" 2>/dev/null)"
want "2. unattr: landstate unattributed field"     "test-extra" \
     "$(awk '{print $5}' "$LANDSTATE/sp-eu-a" 2>/dev/null)"
clean_case

# =============================================================================
# 3. FAIL LINES AND UNATTRIBUTED NOTES COEXIST — repro prints a FAIL line AND
#    the run has unattributed suites; both must appear in the ejection note
#    (law-escalations-carry-their-evidence).
# =============================================================================
testdb_reset
make_branch sp-eu-both spira/canary.sh
make_branch sp-eu-both2 sp-eu-both2.txt
build_batch sp-eu-both sp-eu-both2 > /dev/null
for id in sp-eu-both sp-eu-both2; do plant_bead "$id"; done
printf 'red\nred-suite: test-canary.sh\nred-suite: test-extra-x.sh\n' > "$FORGE_STATUS_FILE"
printf 'spira/sp-eu-both\n' > "$REPRO_FAIL_FILE"
export REPRO_FAIL_LINE="FAIL both-test: expected [ok] got [fail]"
verdict "$REPONAME" > /dev/null
unset REPRO_FAIL_LINE
is   "3. coexist: sp-eu-both ejected"               "EJECTED"          "$(land_state_of sp-eu-both)"
want "3. coexist: note has FAIL line"               "both-test"        "$(notes_of sp-eu-both)"
want "3. coexist: note has unattributed"            "not attributed"   "$(notes_of sp-eu-both)"
want "3. coexist: unattributed names extra suite"   "test-extra-x.sh"  "$(notes_of sp-eu-both)"
clean_case

# =============================================================================
# 4. CONSECUTIVE EJECTION COUNTER — each ejection increments the per-bead
#    counter file in $SPIRA_RUN/eject-count/<id> that landing.sh reads to force
#    a full-corpus gate after two consecutive ejections.
# =============================================================================
testdb_reset
make_branch sp-eu-cnt spira/canary.sh
build_batch sp-eu-cnt > /dev/null
plant_bead sp-eu-cnt
printf 'spira/sp-eu-cnt\n' > "$REPRO_FAIL_FILE"
verdict "$REPONAME" > /dev/null
is "4a. counter: after first ejection, count=1" "1" \
    "$(cat "$RUN/eject-count/sp-eu-cnt" 2>/dev/null)"

tip_cnt="$(awk '{print $2}' "$LANDSTATE/sp-eu-cnt" 2>/dev/null)"
base_sha_cnt="$(git -C "$REPO" rev-parse origin/main)"
batch_br_cnt="spira/queue/cnt2-$$"
git -C "$REPO" branch -f "$batch_br_cnt" "$tip_cnt" 2>/dev/null || true
printf 'pr=70\nhead=%s\nbase=%s\nmembers=sp-eu-cnt:%s\nopened=%s\nbranch=%s\n' \
    "$tip_cnt" "$base_sha_cnt" "$tip_cnt" "$(date +%s)" "$batch_br_cnt" \
    > "$(batch_file)"
printf 'BATCHED %s %s\n' "$tip_cnt" "$(date +%s)" > "$LANDSTATE/sp-eu-cnt"
verdict "$REPONAME" > /dev/null
is "4b. counter: after second ejection, count=2" "2" \
    "$(cat "$RUN/eject-count/sp-eu-cnt" 2>/dev/null)"
clean_case

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
