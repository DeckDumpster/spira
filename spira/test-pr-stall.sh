#!/usr/bin/env bash
#
# test-pr-stall.sh — PR-mode stall detector.
#
#   ./test-pr-stall.sh
#
# WHAT THIS SUITE TESTS
# ---------------------
# sp-790sv adds watchtower --pr-stall-check, which scans spira-lc deliveries
# stuck in PR_OPEN longer than SPIRA_PR_STALL_MINS and acts:
#   checks red        → escalate, deduped per bead (sp-45rmp: tested first — a red
#                        request is the likeliest reason a PR sits past the threshold,
#                        and arming auto-merge on it is a no-op that never fires).
#   allow_auto_merge=false → escalate once per repo via incident.sh (deduped).
#   CONFLICTING       → requeue the delivery so landing rebases on the next pass.
#   otherwise         → arm auto-merge on the PR.
#
# doctor.sh's own allow_auto_merge FAIL (for any land=pr repo with it false) was part
# of the "repositories" section removed by sp-utt1i; repo-map/config validation is the
# config-store-preflight area's job now (sp-n071y), not doctor's.
#
# THE POSITIVE CONTROL IS THE ENTIRE FIRST BLOCK. Before asserting that nothing is
# filed for a recent PR, this suite plants a stale PR_OPEN delivery:<repo> entry and
# requires the incident to fire. A detector that silently passes the "no new incidents"
# test without ever filing one proves nothing (law-absence-needs-a-positive-control).
#
# THE RED-CHECKS PATH IS COVERED FIRST (sp-45rmp: a red request got auto-merge armed
# and was never reported again). The allow_auto_merge=false path is covered second,
# then CONFLICTING, then the arm path.
#
# tier: T1
# covers: watchtower/src/* sentinel/src/* doctor/src/* spira/conf.sh landing-pass/src/* UC-ops-detection-remediation-28 spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
ROOT="$(cd "$HERE/.." && pwd -P)"


echo "test-pr-stall.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run" "$TMP/home"
lc_fix_init "$TMP/lc"

# ---- Fixture: a fake git repo used as the pr-mode repo --------------------------------
FAKE_REPO="$TMP/fake-repo"
git init --quiet -b main "$FAKE_REPO" 2>/dev/null \
    || { git init --quiet "$FAKE_REPO"; git -C "$FAKE_REPO" checkout -qb main 2>/dev/null; } \
    || git init --quiet "$FAKE_REPO"
git -C "$FAKE_REPO" config user.email "test@example.com"
git -C "$FAKE_REPO" config user.name "Test"
printf 'content\n' > "$FAKE_REPO/f"
git -C "$FAKE_REPO" add f
git -C "$FAKE_REPO" commit -q -m "base"

# ---- Fixture: repo-map -----------------------------------------------------------------
REPO_MAP="$TMP/repo-map"
printf 'testrepo|%s|pr|origin/main||\n' "$FAKE_REPO" > "$REPO_MAP"

# ---- Fixture: PR_OPEN delivery rows (entered 90 minutes ago = stale, 10 = fresh) -----------
STALE_EPOCH="$(( $(date +%s) - 5400 ))"
FRESH_EPOCH="$(( $(date +%s) - 600 ))"

branch_in_repo() { git -C "$FAKE_REPO" branch -f "spira/$1" HEAD 2>/dev/null; }
plant_stale() {   # plant_stale <id>
    branch_in_repo "$1"; lc_delivery PR_OPEN "$1" pr "$STALE_EPOCH" 3
}
plant_fresh() {   # plant_fresh <id>
    branch_in_repo "$1"; lc_delivery PR_OPEN "$1" pr "$FRESH_EPOCH" 3
}
plant_certified() {  # plant_certified <id>: a bead in another delivery state
    branch_in_repo "$1"; lc_delivery QUEUED "$1" queue "$STALE_EPOCH" 3
}
requeued() { grep -c "event delivery $1 --expect PR_OPEN --version 3 --actor harness" "$LC_FIX/events.log"; }

# ---- Stub: gh --------------------------------------------------------------------------
# SPIRA_GH is set to a stub binary so no real GitHub calls happen.
# GH_ALLOW_AUTO_MERGE controls what `gh api repos/{owner}/{repo} --jq .allow_auto_merge` returns.
# GH_MERGEABLE / GH_CONCLUSIONS control the combined `pr view --json mergeable,
# statusCheckRollup` jq output (mergeable, then a comma-joined conclusions list).
# GH_PR_MERGE_LOG captures `gh pr merge` calls.
GH_BIN="$TMP/gh-stub"
GH_PR_MERGE_LOG="$TMP/gh-pr-merge.log"

cat > "$GH_BIN" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$*" in
    *"api repos/"*)
        # Simulate `gh api repos/{owner}/{repo} --jq .allow_auto_merge` — output the raw
        # jq-extracted value, not the JSON object; that is what real gh outputs with --jq.
        printf '%s\n' "${GH_ALLOW_AUTO_MERGE:-true}"
        printf '%s\n' "${GH_ALLOW_AUTO_MERGE:-true}" > "${GH_AAM_OUT:-/dev/null}" ;;
    *"pr view"*"--json mergeable,statusCheckRollup"*)
        printf '%s\t%s\n' "${GH_MERGEABLE:-MERGEABLE}" "${GH_CONCLUSIONS:-}" ;;
    *"pr merge"*"--auto"*)
        printf '%s\n' "$*" >> "${GH_PR_MERGE_LOG:-/dev/null}" ;;
    *) : ;;
esac
exit 0
GHEOF
chmod +x "$GH_BIN"

# ---- Stub: incident.sh -----------------------------------------------------------------
INC_STUB="$TMP/mock-inc.sh"
INC_SUBJECTS="$TMP/inc-subjects"
INC_CAUSES="$TMP/inc-causes"
INC_REFS="$TMP/inc-refs"
cat > "$INC_STUB" <<'INCEOF'
#!/usr/bin/env bash
printf '%s\n' "$2"                         >> "${INC_SUBJECTS:-/dev/null}"
printf '%s\n' "${SPIRA_INCIDENT_CAUSE:-}"  >> "${INC_CAUSES:-/dev/null}"
printf '%s\n' "${SPIRA_INCIDENT_REF:-}"    >> "${INC_REFS:-/dev/null}"
cat > /dev/null
INCEOF
chmod +x "$INC_STUB"

# Run --pr-stall-check in an isolated environment.
psc() {  # psc [VAR=val...]
    tl_config SPIRA_RUN="$TMP/run" SPIRA_REPO_MAP="$REPO_MAP" SPIRA_GH="$GH_BIN"
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        SPIRA_HOME="$HERE" \
        SPIRA_LC_BIN="$SPIRA_LC_BIN" LC_FIX="$LC_FIX" \
        GH_LOG="$TMP/gh.log" \
        GH_ALLOW_AUTO_MERGE="${GH_ALLOW_AUTO_MERGE:-false}" \
        GH_MERGEABLE="${GH_MERGEABLE:-MERGEABLE}" \
        GH_CONCLUSIONS="${GH_CONCLUSIONS:-}" \
        GH_AAM_OUT="$TMP/gh-aam-out" \
        GH_PR_MERGE_LOG="$GH_PR_MERGE_LOG" \
        INC_SUBJECTS="$INC_SUBJECTS" \
        INC_CAUSES="$INC_CAUSES" \
        INC_REFS="$INC_REFS" \
        SPIRA_INCIDENT_SH="$INC_STUB" \
        "$@" watchtower --pr-stall-check 2>/dev/null
}

fresh() {
    rm -rf "$TMP/run"
    mkdir -p "$TMP/run"
    lc_fix_init "$TMP/lc"
    git -C "$FAKE_REPO" for-each-ref --format='%(refname:short)' 'refs/heads/spira/*' | while read -r _b; do git -C "$FAKE_REPO" branch -D "$_b" >/dev/null 2>&1; done
    > "$INC_SUBJECTS"; > "$INC_CAUSES"; > "$INC_REFS"
    > "$TMP/gh.log"; > "$GH_PR_MERGE_LOG"
}

# ====================================================================================
echo
echo "positive control: stale PR_OPEN delivery with a failing check → incident filed, not armed"
# THE POSITIVE CONTROL for the red-checks path (sp-45rmp). allow_auto_merge=true and the
# PR is not CONFLICTING — the two branches that existed before this fix — but a check has
# failed, so arming auto-merge would be a no-op that can never fire.
# ====================================================================================
fresh
plant_stale sp-testR
GH_ALLOW_AUTO_MERGE=true GH_MERGEABLE=MERGEABLE GH_CONCLUSIONS=SUCCESS,FAILURE psc
subjects="$(cat "$INC_SUBJECTS" 2>/dev/null)"
causes="$(cat "$INC_CAUSES" 2>/dev/null)"
refs="$(cat "$INC_REFS" 2>/dev/null)"
want   "incident subject mentions PR STALL"          "PR STALL"                    "$subjects"
want   "incident subject mentions the bead"          "sp-testR"                    "$subjects"
want   "cause is pr-stall-checks-red"                 "pr-stall-checks-red"         "$causes"
want   "ref scoped to repo and bead"                  "pr-stall-checks-red:testrepo:sp-testR" "$refs"
nowant "arm command was NOT called"                   "pr merge"                    "$(cat "$GH_PR_MERGE_LOG")"

# ====================================================================================
echo
echo "red-checks deduplication: same bead filed once per ref"
# ====================================================================================
fresh
plant_stale sp-testR
GH_ALLOW_AUTO_MERGE=true GH_CONCLUSIONS=FAILURE psc
GH_ALLOW_AUTO_MERGE=true GH_CONCLUSIONS=FAILURE psc
first_ref="$(head -1 "$INC_REFS" 2>/dev/null)"
second_ref="$(tail -1 "$INC_REFS" 2>/dev/null)"
is "dedup ref is stable across calls" "$first_ref" "$second_ref"

# ====================================================================================
echo
echo "red checks outrank CONFLICTING and allow_auto_merge=false: red is reported"
# Both an older branch (CONFLICTING) and the untouched branch (allow_auto_merge=false)
# would otherwise apply; the red-checks branch is tested first and wins.
# ====================================================================================
fresh
plant_stale sp-testR2
GH_ALLOW_AUTO_MERGE=false GH_MERGEABLE=CONFLICTING GH_CONCLUSIONS=CANCELLED psc
causes="$(cat "$INC_CAUSES" 2>/dev/null)"
want   "cause is pr-stall-checks-red, not auto-merge-off" "pr-stall-checks-red" "$causes"
nowant "auto-merge-off cause not filed"                   "pr-stall-auto-merge-off" "$causes"
is     "NOT requeued (red wins over CONFLICTING)" "0" "$(requeued sp-testR2)"

# ====================================================================================
echo
echo "positive control: stale PR_OPEN delivery with allow_auto_merge=false → incident filed"
# THE POSITIVE CONTROL. Before any absence assertion can be trusted, this block plants a
# stale pr-open entry and requires the incident to fire. A check that never fires is
# indistinguishable from one that fires correctly when nothing triggers it.
# ====================================================================================
fresh
plant_stale sp-test1
GH_ALLOW_AUTO_MERGE=false psc
subjects="$(cat "$INC_SUBJECTS" 2>/dev/null)"
causes="$(cat "$INC_CAUSES" 2>/dev/null)"
refs="$(cat "$INC_REFS" 2>/dev/null)"
want  "incident subject mentions PR STALL"          "PR STALL"                       "$subjects"
want  "incident subject mentions repo name"         "testrepo"                       "$subjects"
want  "cause is pr-stall-auto-merge-off"            "pr-stall-auto-merge-off"        "$causes"
want  "ref scoped to repo"                          "pr-stall-auto-merge-off:testrepo" "$refs"
nowant "arm command was NOT called"                 "pr merge"                       "$(cat "$GH_PR_MERGE_LOG")"

# ====================================================================================
echo
echo "allow_auto_merge=false deduplication: same repo filed once per ref"
# The dedup happens inside incident.sh (external_ref). We verify the ref is stable
# across calls — a count-embedded ref would file a new bead on every pass.
# ====================================================================================
fresh
plant_stale sp-test1
GH_ALLOW_AUTO_MERGE=false psc
GH_ALLOW_AUTO_MERGE=false psc
refs_all="$(cat "$INC_REFS" 2>/dev/null)"
# Both calls should use the same ref (incident.sh dedup handles the rest).
first_ref="$(head -1 "$INC_REFS" 2>/dev/null)"
second_ref="$(tail -1 "$INC_REFS" 2>/dev/null)"
is "dedup ref is stable across calls" "$first_ref" "$second_ref"

# ====================================================================================
echo
echo "allow_auto_merge=true: arm command is issued, no incident filed"
# ====================================================================================
fresh
plant_stale sp-test2
GH_ALLOW_AUTO_MERGE=true psc
arm_log="$(cat "$GH_PR_MERGE_LOG" 2>/dev/null)"
want   "arm command was called"            "pr merge"       "$arm_log"
want   "arm command targets the bead branch" "spira/sp-test2" "$arm_log"
nowant "no incident filed when arming"    "PR STALL"       "$(cat "$INC_SUBJECTS")"

# ====================================================================================
echo
echo "allow_auto_merge=true + CONFLICTING: delivery requeued, arm not called"
# POSITIVE CONTROL for the conflicting-PR path. A CONFLICTING PR will never merge
# until rebased; arming auto-merge does not help. Requeueing the delivery lets
# landing.sh's needs_refresh trigger a rebase on the next pass.
# ====================================================================================
fresh
plant_stale sp-testC
GH_ALLOW_AUTO_MERGE=true GH_MERGEABLE=CONFLICTING psc
is "requeued for CONFLICTING (PrOpen Requeued as the harness)"  "1" "$(requeued sp-testC)"
nowant "no arm call for CONFLICTING"    "pr merge" "$(cat "$GH_PR_MERGE_LOG")"
nowant "no incident for CONFLICTING"    "PR STALL" "$(cat "$INC_SUBJECTS")"

# ====================================================================================
echo
echo "fresh entry (within threshold): nothing is filed or armed"
# This absence test is valid because the positive control above proved the detector fires.
# ====================================================================================
fresh
plant_fresh sp-test3
GH_ALLOW_AUTO_MERGE=false psc
is "no incident for fresh entry"     "" "$(cat "$INC_SUBJECTS" | tr -d '\n')"
is "no arm call for fresh entry"     "" "$(cat "$GH_PR_MERGE_LOG" | tr -d '\n')"

# ====================================================================================
echo
echo "a delivery not in PR_OPEN is not flagged"
# ====================================================================================
fresh
plant_certified sp-test4
GH_ALLOW_AUTO_MERGE=false psc
is "CERTIFIED entry not flagged"   "" "$(cat "$INC_SUBJECTS" | tr -d '\n')"

# ====================================================================================
echo
echo "no registered repo carries the branch: skipped without error"
# A PR_OPEN row whose branch no registered checkout carries is skipped, not an error.
# ====================================================================================
fresh
lc_delivery PR_OPEN sp-test5 pr "$STALE_EPOCH" 3   # no checkout carries its branch
GH_ALLOW_AUTO_MERGE=false psc; rc=$?
is "exits 0 for unknown repo"    "0" "$rc"
is "no incident for unknown repo" "" "$(cat "$INC_SUBJECTS" | tr -d '\n')"

# ====================================================================================
echo
echo "multiple stale entries: each repo escalated separately"
# ====================================================================================
fresh
# Two entries for testrepo → only one escalation (same ref, dedup in incident.sh).
plant_stale sp-testA
plant_stale sp-testB
GH_ALLOW_AUTO_MERGE=false psc
refs_count="$(cat "$INC_REFS" | grep -c 'pr-stall-auto-merge-off:testrepo' || true)"
want "two stale entries in same repo still use the same ref" \
    "pr-stall-auto-merge-off:testrepo" "$(cat "$INC_REFS")"

# ====================================================================================
echo
echo "spira-lc unreachable: nothing is filed, armed or requeued"
# ====================================================================================
fresh
plant_stale sp-testU
SPIRA_LC_BIN="$TMP/absent" GH_ALLOW_AUTO_MERGE=false GH_MERGEABLE=CONFLICTING psc; rc=$?
is "exits 0 when spira-lc is unreachable" "0" "$rc"
is "no incident when spira-lc is unreachable" "" "$(cat "$INC_SUBJECTS" | tr -d '\n')"
is "no requeue when spira-lc is unreachable" "0" "$(requeued sp-testU)"

# ====================================================================================
echo
echo "conf.sh: SPIRA_PR_STALL_MINS is a settable key"
# ====================================================================================
conf_out="$(env -i SPIRA_CONF=/nonexistent SPIRA_REPO="$ROOT" PATH="$PATH" \
    SPIRA_TOML="$_TL_CONF_BASE" \
    bash -c '. '"$HERE"'/conf.sh; echo "stall=${SPIRA_PR_STALL_MINS}"' 2>/dev/null || true)"
want  "SPIRA_PR_STALL_MINS has the fixture's default value of 60" "stall=60" "$conf_out"

tl_config SPIRA_PR_STALL_MINS=30
conf_out2="$(env -i SPIRA_CONF=/nonexistent SPIRA_REPO="$ROOT" PATH="$PATH" \
    SPIRA_TOML="$SPIRA_TOML" \
    bash -c '. '"$HERE"'/conf.sh; echo "stall=${SPIRA_PR_STALL_MINS}"' 2>/dev/null || true)"
want "SPIRA_PR_STALL_MINS is overridable via config" "stall=30" "$conf_out2"

# ====================================================================================
echo
tl_summary
