#!/usr/bin/env bash
#
# test-pr-stall.sh — PR-mode stall detector and doctor.sh allow_auto_merge check.
#
#   ./test-pr-stall.sh
#
# WHAT THIS SUITE TESTS
# ---------------------
# sp-790sv adds watchtower.sh --pr-stall-check, which scans landstate files for
# REBASED pr-open:<repo> entries older than SPIRA_PR_STALL_MINS and acts:
#   allow_auto_merge=false → escalate once per repo via incident.sh (deduped).
#   allow_auto_merge=true  → arm auto-merge on the PR.
#
# It also adds a doctor.sh FAIL for any land=pr repo with allow_auto_merge=false.
#
# THE POSITIVE CONTROL IS THE ENTIRE FIRST BLOCK. Before asserting that nothing is
# filed for a recent PR, this suite plants a stale REBASED pr-open:<repo> entry and
# requires the incident to fire. A detector that silently passes the "no new incidents"
# test without ever filing one proves nothing (law-absence-needs-a-positive-control).
#
# THE ALLOW_AUTO_MERGE=FALSE PATH IS COVERED FIRST (it is the case that blocked
# seven beads for 36 hours). The arm path is covered second.
#
# covers: spira/watchtower.sh spira/sentinel.sh spira/doctor.sh spira/conf.sh spira/landing.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "[$2] not in output"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "[$2] in output unexpectedly" ;; *) ok "$1"; esac; }

echo "test-pr-stall.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
mkdir -p "$TMP/run/landstate" "$TMP/home"

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

# ---- Fixture: stale landstate (REBASED pr-open:testrepo, 90 minutes old) ---------------
STALE_EPOCH="$(( $(date +%s) - 5400 ))"
FRESH_EPOCH="$(( $(date +%s) - 600 ))"

plant_stale() {   # plant_stale <id> [repo]
    printf 'REBASED abc123def456 %s pr-open:%s' "$STALE_EPOCH" "${2:-testrepo}" \
        > "$TMP/run/landstate/$1"
}
plant_fresh() {   # plant_fresh <id>
    printf 'REBASED abc123def456 %s pr-open:testrepo' "$FRESH_EPOCH" \
        > "$TMP/run/landstate/$1"
}
plant_certified() {  # plant_certified <id>
    printf 'CERTIFIED abc123def456 %s' "$STALE_EPOCH" \
        > "$TMP/run/landstate/$1"
}

# ---- Stub: gh --------------------------------------------------------------------------
# SPIRA_GH is set to a stub binary so no real GitHub calls happen.
# GH_ALLOW_AUTO_MERGE controls what `gh repo view --json allowAutoMerge` returns.
# GH_PR_MERGE_LOG captures `gh pr merge` calls.
GH_BIN="$TMP/gh-stub"
GH_PR_MERGE_LOG="$TMP/gh-pr-merge.log"

cat > "$GH_BIN" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$*" in
    *"--jq .allowAutoMerge"*)
        # Simulate `gh repo view --json allowAutoMerge --jq .allowAutoMerge` — output the raw
        # jq-extracted value, not the JSON object; that is what real gh outputs with --jq.
        printf '%s\n' "${GH_ALLOW_AUTO_MERGE:-true}"
        printf '%s\n' "${GH_ALLOW_AUTO_MERGE:-true}" > "${GH_AAM_OUT:-/dev/null}" ;;
    *"pr view"*"--json mergeable"*)
        printf '%s\n' "${GH_MERGEABLE:-MERGEABLE}" ;;
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
    env -i PATH="$PATH" HOME="$TMP/home" \
        SPIRA_CONF=/nonexistent \
        SPIRA_RUN="$TMP/run" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_GH="$GH_BIN" \
        GH_LOG="$TMP/gh.log" \
        GH_ALLOW_AUTO_MERGE="${GH_ALLOW_AUTO_MERGE:-false}" \
        GH_MERGEABLE="${GH_MERGEABLE:-MERGEABLE}" \
        GH_AAM_OUT="$TMP/gh-aam-out" \
        GH_PR_MERGE_LOG="$GH_PR_MERGE_LOG" \
        INC_SUBJECTS="$INC_SUBJECTS" \
        INC_CAUSES="$INC_CAUSES" \
        INC_REFS="$INC_REFS" \
        SPIRA_INCIDENT_SH="$INC_STUB" \
        "$@" bash "$HERE/watchtower.sh" --pr-stall-check 2>/dev/null
}

fresh() {
    rm -rf "$TMP/run"
    mkdir -p "$TMP/run/landstate"
    > "$INC_SUBJECTS"; > "$INC_CAUSES"; > "$INC_REFS"
    > "$TMP/gh.log"; > "$GH_PR_MERGE_LOG"
}

# ====================================================================================
echo
echo "positive control: stale REBASED pr-open with allow_auto_merge=false → incident filed"
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
echo "allow_auto_merge=true + CONFLICTING: landstate cleared, arm not called"
# POSITIVE CONTROL for the conflicting-PR path. A CONFLICTING PR will never merge
# until rebased; arming auto-merge does not help. Clearing the landstate lets
# landing.sh's needs_refresh trigger a rebase on the next pass.
# ====================================================================================
fresh
plant_stale sp-testC
GH_ALLOW_AUTO_MERGE=true GH_MERGEABLE=CONFLICTING psc
is "landstate cleared for CONFLICTING"  "" "$([ -f "$TMP/run/landstate/sp-testC" ] && echo exists)"
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
echo "non-pr-open landstate: CERTIFIED entry is not flagged"
# ====================================================================================
fresh
plant_certified sp-test4
GH_ALLOW_AUTO_MERGE=false psc
is "CERTIFIED entry not flagged"   "" "$(cat "$INC_SUBJECTS" | tr -d '\n')"

# ====================================================================================
echo
echo "unknown repo in landstate: skipped without error"
# A REBASED pr-open:<repo> where repo is not in the repo-map is silently skipped —
# the repo_root call fails and the entry is ignored rather than erroring.
# ====================================================================================
fresh
printf 'REBASED abc123 %s pr-open:unknownrepo' "$STALE_EPOCH" \
    > "$TMP/run/landstate/sp-test5"
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
echo "doctor.sh: land=pr repo with allow_auto_merge=false → FAIL"
# ====================================================================================
# We can only run the repositories section check when gh is available and faked.
# Build a minimal doctor environment: stub bd, stub systemctl, fake db, stub gh.
DOCTOR_TMP="$TMP/doctor"
mkdir -p "$DOCTOR_TMP/db/.beads" "$DOCTOR_TMP/run" "$DOCTOR_TMP/home/.config/systemd/user"

# Stub bd (schema check passes, list returns empty)
cat > "$DOCTOR_TMP/bd" <<'BDEOF'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*) printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*)           printf '[]\n'; exit 0 ;;
    *"version"*)        printf 'bd v1.2.2\n'; exit 0 ;;
    *)                  exit 0 ;;
esac
BDEOF
chmod +x "$DOCTOR_TMP/bd"

# Stub systemctl (all units enabled and active)
cat > "$DOCTOR_TMP/systemctl" <<'SCEOF'
#!/usr/bin/env bash
case "$*" in
    *"is-active"*"--quiet"*) exit 0 ;;
    *"is-active"*)           printf 'active\n' ;;
    *"is-enabled"*)          printf 'enabled\n'; exit 0 ;;
    *"list-units"*)          : ;;
esac
exit 0
SCEOF
chmod +x "$DOCTOR_TMP/systemctl"

# Fake dolt (not needed for the repo check, but doctor.sh checks it)
cat > "$DOCTOR_TMP/dolt" <<'DOLTEOF'
#!/usr/bin/env bash
exit 0
DOLTEOF
chmod +x "$DOCTOR_TMP/dolt"

# gh stub: return allow_auto_merge=false for the doctor check
cat > "$DOCTOR_TMP/gh" <<'GHEOF'
#!/usr/bin/env bash
case "$*" in
    *"--json allowAutoMerge"*)
        printf '%s\n' "${GH_ALLOW_AUTO_MERGE:-false}" ;;
    *) exit 0 ;;
esac
GHEOF
chmod +x "$DOCTOR_TMP/gh"

DOCTOR_CONF="$DOCTOR_TMP/spira.conf"
printf 'SPIRA_RUN = %s\nSPIRA_DB = %s/db\nSPIRA_PATH = %s\nSPIRA_OPERATED = 0\n' \
    "$DOCTOR_TMP/run" "$DOCTOR_TMP" "$DOCTOR_TMP" > "$DOCTOR_CONF"

DOCTOR_MAP="$DOCTOR_TMP/repo-map"
printf 'prrepo|%s|pr|origin/main||\n' "$FAKE_REPO" > "$DOCTOR_MAP"

run_doctor() {
    env -i PATH="$DOCTOR_TMP:$PATH" HOME="$DOCTOR_TMP/home" \
        GH_ALLOW_AUTO_MERGE="${GH_ALLOW_AUTO_MERGE:-false}" \
        SPIRA_CONF="$DOCTOR_CONF" \
        SPIRA_REPO_MAP="$DOCTOR_MAP" \
        SPIRA_SYSTEMCTL="$DOCTOR_TMP/systemctl" \
        SPIRA_REPO="$ROOT" \
        bash "$HERE/doctor.sh" 2>/dev/null
}

# POSITIVE CONTROL FOR DOCTOR.SH: allow_auto_merge=false → FAIL.
GH_ALLOW_AUTO_MERGE=false
dr_out="$(run_doctor)"
want  "doctor FAIL for allow_auto_merge=false"  "FAIL" "$dr_out"
want  "doctor names the repo"                   "prrepo" "$dr_out"
want  "doctor mentions allow_auto_merge"        "allow_auto_merge" "$dr_out"

# NEGATIVE: allow_auto_merge=true → ok.
GH_ALLOW_AUTO_MERGE=true
dr_out="$(run_doctor)"
nowant "doctor ok for allow_auto_merge=true"    "FAIL" "$(printf '%s\n' "$dr_out" | grep 'allow_auto_merge')"

# ====================================================================================
echo
echo "conf.sh: SPIRA_PR_STALL_MINS is a settable key"
# ====================================================================================
conf_out="$(env -i SPIRA_CONF=/nonexistent SPIRA_REPO="$ROOT" PATH="$PATH" \
    bash -c '. '"$HERE"'/conf.sh; echo "stall=${SPIRA_PR_STALL_MINS}"' 2>/dev/null || true)"
want  "SPIRA_PR_STALL_MINS has default value of 60" "stall=60" "$conf_out"

conf_out2="$(env -i SPIRA_CONF=/nonexistent SPIRA_REPO="$ROOT" PATH="$PATH" \
    SPIRA_PR_STALL_MINS=30 \
    bash -c '. '"$HERE"'/conf.sh; echo "stall=${SPIRA_PR_STALL_MINS}"' 2>/dev/null || true)"
want "SPIRA_PR_STALL_MINS is overridable from env" "stall=30" "$conf_out2"

# ====================================================================================
echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
