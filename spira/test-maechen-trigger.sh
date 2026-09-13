#!/usr/bin/env bash
#
# test-maechen-trigger.sh — maechen-trigger.sh: dual-trigger conditions, shared
#   watermark, and idempotent deduplication.
#
#   ./test-maechen-trigger.sh
#
# WHAT THIS SUITE IS GUARDING
# ---------------------------
# maechen-trigger.sh fires a sweep bead when EITHER of two conditions is met:
#
#   1. LANDING TRIGGER: commits naming a bead id on managed repos' base branches
#      have accumulated to SPIRA_MAECHEN_LANDING_INTERVAL since the watermark.
#
#   2. TIME TRIGGER: SPIRA_MAECHEN_MAX_GAP_SECONDS have elapsed since the watermark.
#
# Properties guarded here:
#
#   a. Time trigger fires when the gap threshold is exceeded.
#   b. Landing trigger fires when the landing count threshold is reached.
#   c. Dedup: when an open trigger bead exists, no second bead is filed.
#   d. No trigger: when neither condition is met, no bead is filed.
#   e. The watermark is advanced (file written) before the bead is filed.
#   f. The labels used match SPIRA_SCOPE_LABEL + SPIRA_MAECHEN_LABEL so the fayth
#      predicate finds the trigger bead.
#   g. Both triggers firing at once produce ONE bead, not two (shared watermark).
#   h. bd create failure is reported and exits 1.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
# ---------------------------------------------------------
# For each trigger condition, the suite verifies the ABSENCE of the trigger by showing
# the condition NOT met (watermark=now, count=0) before testing the presence case.
# The dedup check is verified by an explicit open-bead response that blocks a `create`.
#
# STUB BD (law-gates-run-in-a-clean-environment)
# -----------------------------------------------
# bd calls are stubbed so no database is needed. Real git is used for landing-count
# tests against throwaway repos (law-prefer-the-real-dependency).
#
# covers: spira/maechen-trigger.sh spira/conf.sh
# hermetic-ok: stub bd, real git with throwaway repos
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { case "$3" in *"$2"*) ok "$1" ;; *) bad "$1" "wanted [$2] in [$3]"; esac; }
nowant() { case "$3" in *"$2"*) bad "$1" "did not want [$2] in [$3]" ;; *) ok "$1" ;; esac; }

TRIGSH="$HERE/maechen-trigger.sh"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
NONE="$T/none.conf"

# ---------------------------------------------------------------------------
# STUB BD. Records argv to BD_LOG_PATH; returns BD_LIST_OUTPUT for `list`
# subcommands; exits 0 for all others. Strips `-C <db>` prefix so the
# subcommand is always $1 at dispatch time (same pattern as test-groom-trigger.sh).
# ---------------------------------------------------------------------------
STUB_BD="$T/stub-bd"
BD_LOG="$T/bd.log"
cat > "$STUB_BD" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$BD_LOG_PATH"
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '%s\n' "${BD_LIST_OUTPUT:-[]}"; exit 0 ;;
    *)    exit 0 ;;
esac
STUB
chmod +x "$STUB_BD"

FAIL_BD="$T/fail-bd"
cat > "$FAIL_BD" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "-C" ] && shift 2
case "${1:-}" in
    list) printf '[]'; exit 0 ;;
    *)    exit 1 ;;
esac
STUB
chmod +x "$FAIL_BD"

# ---------------------------------------------------------------------------
# THROWAWAY GIT REPO. Commits added here are counted by the landing trigger.
# origin/main is faked via a direct ref write (no bare remote needed).
# ---------------------------------------------------------------------------
TESTREPO="$T/testrepo"
git init -q "$TESTREPO"
git -C "$TESTREPO" config user.email "test@example.com"
git -C "$TESTREPO" config user.name "Test"
git -C "$TESTREPO" commit --allow-empty -q -m "initial"
mkdir -p "$TESTREPO/.git/refs/remotes/origin"
git -C "$TESTREPO" rev-parse HEAD > "$TESTREPO/.git/refs/remotes/origin/main"

# add_landing <subject> — add a commit that the landing trigger counts.
add_landing() {
    git -C "$TESTREPO" commit --allow-empty -q -m "$1"
    git -C "$TESTREPO" rev-parse HEAD > "$TESTREPO/.git/refs/remotes/origin/main"
}

RUNDIR="$T/run"
mkdir -p "$RUNDIR"
WATERMARK_FILE="$RUNDIR/maechen.watermark"

now_ts="$(date +%s)"

# Base environment shared by all runs. Individual tests override variables as needed.
run_trigger() {
    : > "$BD_LOG"
    env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="${SPIRA_BD_OVERRIDE:-$STUB_BD}" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="${BD_LIST_OUTPUT:-[]}" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUNDIR" \
        SPIRA_REPO="$TESTREPO" \
        SPIRA_MAECHEN_LABEL="${SPIRA_MAECHEN_LABEL:-maechen-sweep}" \
        SPIRA_SCOPE_LABEL="${SPIRA_SCOPE_LABEL:-spira}" \
        SPIRA_MAECHEN_LANDING_INTERVAL="${SPIRA_MAECHEN_LANDING_INTERVAL:-25}" \
        SPIRA_MAECHEN_MAX_GAP_SECONDS="${SPIRA_MAECHEN_MAX_GAP_SECONDS:-10800}" \
        bash "$TRIGSH" 2>&1
}

# ==========================================================================================
echo
echo "TIME TRIGGER: no-trigger control — watermark is 'now', gap threshold is large"
# ==========================================================================================
# POSITIVE CONTROL: show the trigger does NOT fire when conditions are not met.
# Watermark is the current time; no time has elapsed; gap threshold is huge.
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
: > "$BD_LOG"
SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    out="$(run_trigger)"; rc=$?
is   "no-trigger exits 0"        0 "$rc"
nowant "no create when no trigger" "create" "$(cat "$BD_LOG")"
want   "no-trigger logs 'no trigger'" "no trigger" "$out"

# ==========================================================================================
echo
echo "TIME TRIGGER: fires when SPIRA_MAECHEN_MAX_GAP_SECONDS elapsed"
# ==========================================================================================
# Watermark = 0 (epoch zero); elapsed = now, which far exceeds any threshold.
printf '0\n' > "$WATERMARK_FILE"
SPIRA_MAECHEN_MAX_GAP_SECONDS=60 \
SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    out="$(run_trigger)"; rc=$?
is   "time trigger exits 0"             0        "$rc"
want "bd create is called"              "create" "$(cat "$BD_LOG")"
want "labels include scope label"       "spira"  "$(cat "$BD_LOG")"
want "labels include maechen-sweep"     "maechen-sweep" "$(cat "$BD_LOG")"
want "reason mentions elapsed"          "elapsed" "$out"

# Watermark must have been advanced (written to file with a timestamp near now).
new_wm="$(cat "$WATERMARK_FILE" 2>/dev/null | tr -d '[:space:]' || true)"
is "watermark is non-empty after fire" "1" "$([ -n "$new_wm" ] && echo 1 || echo 0)"
[ "${new_wm:-0}" -ge "$now_ts" ] && ok "watermark is >= trigger start time" \
    || bad "watermark is >= trigger start time" "got $new_wm expected >= $now_ts"

# ==========================================================================================
echo
echo "LANDING TRIGGER: no-trigger control — no bead-naming commits, high threshold"
# ==========================================================================================
# POSITIVE CONTROL: no commits with bead-id subjects exist; threshold is 999.
# Watermark is current time so time trigger doesn't also fire.
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    out="$(run_trigger)"; rc=$?
is   "landing no-trigger exits 0"         0 "$rc"
nowant "no create on landing no-trigger"  "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "LANDING TRIGGER: fires when landing count reaches threshold"
# ==========================================================================================
# Add enough bead-naming commits to exceed a threshold of 2.
# Watermark is set just before the commits so --after=@<wm> includes them.
# git --after='@N' treats N as a Unix timestamp only for N >= 100000000; for N=0 or
# other small values approxidate falls back to 'now', silently finding no commits.
printf '%d\n' "$(( $(date +%s) - 1 ))" > "$WATERMARK_FILE"
add_landing "sp-aaa1: first landing"
add_landing "sp-bbb2: second landing"
SPIRA_MAECHEN_MAX_GAP_SECONDS=999999 \
SPIRA_MAECHEN_LANDING_INTERVAL=2 \
    out="$(run_trigger)"; rc=$?
is   "landing trigger exits 0"           0        "$rc"
want "bd create called on landing"       "create" "$(cat "$BD_LOG")"
want "reason mentions landings"          "landings" "$out"

# ==========================================================================================
echo
echo "LANDING TRIGGER: non-bead commits do not count"
# ==========================================================================================
# Only subjects that start with the bead id prefix count. A subject like "fix typo" must
# not be counted. One matching commit + one non-matching: with threshold=2, should not fire.
# Watermark = now so the time trigger cannot fire (elapsed ~= 0, well below any threshold).
NONMATCH_REPO="$T/nonmatch"
git init -q "$NONMATCH_REPO"
git -C "$NONMATCH_REPO" config user.email "test@example.com"
git -C "$NONMATCH_REPO" config user.name "Test"
git -C "$NONMATCH_REPO" commit --allow-empty -q -m "sp-xyz9: real landing"
git -C "$NONMATCH_REPO" commit --allow-empty -q -m "fix typo in readme"
mkdir -p "$NONMATCH_REPO/.git/refs/remotes/origin"
git -C "$NONMATCH_REPO" rev-parse HEAD > "$NONMATCH_REPO/.git/refs/remotes/origin/main"

# Watermark = now so elapsed = 0 and the time trigger does not fire independently.
# Use a far-future gap threshold to doubly ensure only the landing trigger is tested.
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
: > "$BD_LOG"
out="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="[]" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUNDIR" \
        SPIRA_REPO="$NONMATCH_REPO" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_SCOPE_LABEL="spira" \
        SPIRA_MAECHEN_LANDING_INTERVAL=2 \
        SPIRA_MAECHEN_MAX_GAP_SECONDS=9999999999 \
        bash "$TRIGSH" 2>&1)"; rc=$?
is   "non-matching commits do not fire"   0 "$rc"
nowant "no create for non-matching"       "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "DEDUP: open trigger bead — no second bead is filed"
# ==========================================================================================
# POSITIVE CONTROL: BD_LIST_OUTPUT is a non-empty JSON array (bead already open). The
# trigger must NOT file another bead. The landing and time thresholds are set low so
# conditions would otherwise be met; the dedup alone prevents filing.
printf '0\n' > "$WATERMARK_FILE"
BD_LIST_OUTPUT='[{"id":"sp-test","title":"Maechen pass"}]'
SPIRA_MAECHEN_MAX_GAP_SECONDS=0
SPIRA_MAECHEN_LANDING_INTERVAL=0
out="$(run_trigger)"; rc=$?
unset BD_LIST_OUTPUT SPIRA_MAECHEN_MAX_GAP_SECONDS SPIRA_MAECHEN_LANDING_INTERVAL
is     "dedup exits 0"               0           "$rc"
nowant "dedup does not create"       "create"    "$(cat "$BD_LOG")"
want   "dedup logs skipping"         "skipping"  "$out"
want   "list IS called for dedup"    "list"      "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "BOTH TRIGGERS: both conditions met — exactly ONE bead filed"
# ==========================================================================================
# Watermark=0, low thresholds for both. Only one bd create call must appear.
printf '0\n' > "$WATERMARK_FILE"
SPIRA_MAECHEN_MAX_GAP_SECONDS=0 \
SPIRA_MAECHEN_LANDING_INTERVAL=1 \
    out="$(run_trigger)"; rc=$?
is   "both triggers exits 0"           0 "$rc"
want "bd create called once"           "create" "$(cat "$BD_LOG")"
# Count the number of create calls: must be exactly 1.
create_count="$(grep -c 'create' "$BD_LOG" || true)"
is   "exactly one create call"         "1" "$create_count"

# ==========================================================================================
echo
echo "ERROR: bd create fails — exit code is 1"
# ==========================================================================================
printf '0\n' > "$WATERMARK_FILE"
SPIRA_MAECHEN_MAX_GAP_SECONDS=0 \
SPIRA_MAECHEN_LANDING_INTERVAL=999 \
    out="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$FAIL_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="[]" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUNDIR" \
        SPIRA_REPO="$TESTREPO" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_SCOPE_LABEL="spira" \
        SPIRA_MAECHEN_MAX_GAP_SECONDS=0 \
        SPIRA_MAECHEN_LANDING_INTERVAL=999 \
        bash "$TRIGSH" 2>&1)"; rc=$?
is   "bd create failure exits 1"       1        "$rc"
want "failure log mentions ERROR"      "ERROR"  "$out"

# ==========================================================================================
echo
echo "LABELS: custom SPIRA_SCOPE_LABEL and SPIRA_MAECHEN_LABEL are used"
# ==========================================================================================
# Pin to non-defaults (law-gates-run-in-a-clean-environment). A test asserting the
# default passes even if the code has the literal written in.
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
: > "$BD_LOG"
out="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="[]" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUNDIR" \
        SPIRA_REPO="$TESTREPO" \
        SPIRA_SCOPE_LABEL="myproject" \
        SPIRA_MAECHEN_LABEL="retro" \
        SPIRA_MAECHEN_MAX_GAP_SECONDS=0 \
        SPIRA_MAECHEN_LANDING_INTERVAL=999 \
        bash "$TRIGSH" 2>&1)"; rc=$?
is   "custom labels exits 0"                       0            "$rc"
want "custom scope label in create"                "myproject"  "$(cat "$BD_LOG")"
want "custom maechen label in create"              "retro"      "$(cat "$BD_LOG")"
# The --label argument must not include the old defaults when overridden.
# Extract only the --label value from the create call in this run (log is clean).
label_arg="$(grep 'create' "$BD_LOG" | grep -oP '(?<=--label )\S+' | head -1 || true)"
nowant "default scope label not in --label" "spira"         "${label_arg:-}"
nowant "default maechen label not in --label" "maechen-sweep" "${label_arg:-}"

# ==========================================================================================
echo
echo "LABELS: empty SPIRA_SCOPE_LABEL — no leading comma"
# ==========================================================================================
printf '%d\n' "$now_ts" > "$WATERMARK_FILE"
: > "$BD_LOG"
out="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
        SPIRA_CONF="$NONE" \
        SPIRA_BD="$STUB_BD" \
        BD_LOG_PATH="$BD_LOG" \
        BD_LIST_OUTPUT="[]" \
        SPIRA_DB="$T/fixture.db" \
        SPIRA_RUN="$RUNDIR" \
        SPIRA_REPO="$TESTREPO" \
        SPIRA_SCOPE_LABEL="" \
        SPIRA_MAECHEN_LABEL="maechen-sweep" \
        SPIRA_MAECHEN_MAX_GAP_SECONDS=0 \
        SPIRA_MAECHEN_LANDING_INTERVAL=999 \
        bash "$TRIGSH" 2>&1)"; rc=$?
is     "empty scope exits 0"                0 "$rc"
nowant "no leading comma in --label"        ",maechen-sweep" "$(grep 'create' "$BD_LOG" || true)"
want   "maechen label present without scope" "maechen-sweep" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "REGRESSION sp-b4t: log() stdout capture — no-origin repo followed by counted repo"
# ==========================================================================================
# When _count_landings is called inside $(...) for a repo with no origin remote, the
# skip-path calls log() then prints '0'. With log() writing to stdout (the bug), both the
# log line and '0' are captured into _n.  Bash arithmetic $(( landing_count + _n )) then
# sees the timestamp token "2026-09" and fails with "value too great for base (error token
# is '09')". Fix: log() must redirect to stderr so only the numeric result is captured.
#
# POSITIVE CONTROL: no-origin repo appears before a repo that has landings.
# Before fix: "value too great for base" appears in the combined output.
# After fix:  no error; COUNTED_REPO's landings are cleanly tallied.

# Watermark is recorded before the landing commits are made. git --after='@N' only treats N
# as a Unix timestamp when N >= 100000000 (9 digits); smaller values are parsed as
# approximate/relative dates and resolve to ~now, finding no commits. Using the current
# timestamp avoids that, and the elapsed time (~seconds) is far below MAX_GAP=9999999999.
printf '%d\n' "$(( $(date +%s) - 1 ))" > "$WATERMARK_FILE"

NOREMOTE_B4T="$T/noremote-b4t"
git init -q "$NOREMOTE_B4T"
git -C "$NOREMOTE_B4T" config user.email "test@example.com"
git -C "$NOREMOTE_B4T" config user.name "Test"
git -C "$NOREMOTE_B4T" commit --allow-empty -q -m "initial"
# No origin remote — _count_landings logs "cannot resolve base ref" and returns 0.

COUNTED_B4T="$T/counted-b4t"
git init -q "$COUNTED_B4T"
git -C "$COUNTED_B4T" config user.email "test@example.com"
git -C "$COUNTED_B4T" config user.name "Test"
git -C "$COUNTED_B4T" commit --allow-empty -q -m "initial"
git -C "$COUNTED_B4T" commit --allow-empty -q -m "sp-r1a1: first landing"
git -C "$COUNTED_B4T" commit --allow-empty -q -m "sp-r2b2: second landing"
mkdir -p "$COUNTED_B4T/.git/refs/remotes/origin"
git -C "$COUNTED_B4T" rev-parse HEAD > "$COUNTED_B4T/.git/refs/remotes/origin/main"

HOME_B4T="$T/home-b4t"
git init -q "$HOME_B4T"
git -C "$HOME_B4T" config user.email "test@example.com"
git -C "$HOME_B4T" config user.name "Test"
git -C "$HOME_B4T" commit --allow-empty -q -m "initial"
mkdir -p "$HOME_B4T/.git/refs/remotes/origin"
git -C "$HOME_B4T" rev-parse HEAD > "$HOME_B4T/.git/refs/remotes/origin/main"

REPOMAP_B4T="$T/repomap-b4t"
# no-origin repo listed before the counted repo — the problematic ordering.
printf 'noremote|%s|\ncounted|%s|\n' "$NOREMOTE_B4T" "$COUNTED_B4T" > "$REPOMAP_B4T"
: > "$BD_LOG"
out_b4t="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$STUB_BD" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_DB="$T/fixture.db" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$HOME_B4T" \
    SPIRA_REPO_MAP="$REPOMAP_B4T" \
    SPIRA_MAECHEN_LABEL="maechen-sweep" \
    SPIRA_SCOPE_LABEL="spira" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=9999999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=2 \
    bash "$TRIGSH" 2>&1)"; rc_b4t=$?
# (a) The arithmetic-error message must not appear — log() must not pollute $(...) capture.
nowant "no 'value too great for base' from log stdout capture" \
    "value too great for base" "$out_b4t"
# (b) The trigger fires — COUNTED_REPO's 2 landings correctly reach the threshold of 2.
is   "trigger exits 0 after no-origin repo in map" 0 "$rc_b4t"
want "bd create called — counted repo after no-origin repo was tallied" \
    "create" "$(cat "$BD_LOG")"

# ==========================================================================================
echo
echo "REGRESSION sp-3ljk: non-origin remote — landings must be counted"
# ==========================================================================================
# DEFECT: _count_landings hardcoded 'origin' in both branches of the base-ref resolver.
# A repository whose remote is named anything else (e.g. 'gitea') resolves no base ref,
# logs "cannot resolve base ref", and its landings are silently counted as zero.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control):
#   Before fix: "cannot resolve base ref" appears in the output; landing trigger does not fire.
#   After fix:  trigger fires; no "cannot resolve base ref" in the output.
#
# Setup: a local bare repo acts as the 'gitea' remote so that spira_landref's
# `git remote set-head gitea --auto` resolves without network access.

# Watermark is recorded before any landing commits are made so --after=@<wm> finds them.
# See the sp-b4t watermark comment above for why @0 does not work here.
printf '%d\n' "$(( $(date +%s) - 1 ))" > "$WATERMARK_FILE"

BARE_3LJK="$T/bare-3ljk"
git init -q --bare "$BARE_3LJK"
git -C "$BARE_3LJK" symbolic-ref HEAD refs/heads/master

GITEA_WORK="$T/gitea-work-3ljk"
git init -q "$GITEA_WORK"
git -C "$GITEA_WORK" config user.email "test@example.com"
git -C "$GITEA_WORK" config user.name "Test"
git -C "$GITEA_WORK" commit --allow-empty -q -m "initial"
git -C "$GITEA_WORK" commit --allow-empty -q -m "sp-alt1: first landing on gitea remote"
git -C "$GITEA_WORK" commit --allow-empty -q -m "sp-alt2: second landing on gitea remote"
git -C "$GITEA_WORK" remote add gitea "$BARE_3LJK"
git -C "$GITEA_WORK" push -q gitea master:master

GITEA_REPO="$T/gitea-repo-3ljk"
git clone -q -o gitea "$BARE_3LJK" "$GITEA_REPO"
git -C "$GITEA_REPO" config user.email "test@example.com"
git -C "$GITEA_REPO" config user.name "Test"

HOME_3LJK="$T/home-3ljk"
git init -q "$HOME_3LJK"
git -C "$HOME_3LJK" config user.email "test@example.com"
git -C "$HOME_3LJK" config user.name "Test"
git -C "$HOME_3LJK" commit --allow-empty -q -m "initial"
mkdir -p "$HOME_3LJK/.git/refs/remotes/origin"
git -C "$HOME_3LJK" rev-parse HEAD > "$HOME_3LJK/.git/refs/remotes/origin/main"

REPOMAP_3LJK="$T/repomap-3ljk"
printf 'gitea-repo|%s|\n' "$GITEA_REPO" > "$REPOMAP_3LJK"
: > "$BD_LOG"
out_3ljk="$(env -i HOME="$T" PATH="$HERE:/usr/bin:/bin:/usr/lib/git-core" \
    SPIRA_CONF="$NONE" \
    SPIRA_BD="$STUB_BD" \
    BD_LOG_PATH="$BD_LOG" \
    BD_LIST_OUTPUT="[]" \
    SPIRA_DB="$T/fixture.db" \
    SPIRA_RUN="$RUNDIR" \
    SPIRA_REPO="$HOME_3LJK" \
    SPIRA_REPO_MAP="$REPOMAP_3LJK" \
    SPIRA_MAECHEN_LABEL="maechen-sweep" \
    SPIRA_SCOPE_LABEL="spira" \
    SPIRA_MAECHEN_MAX_GAP_SECONDS=9999999999 \
    SPIRA_MAECHEN_LANDING_INTERVAL=2 \
    bash "$TRIGSH" 2>&1)"; rc_3ljk=$?
nowant "no 'cannot resolve base ref' for gitea-remote repo" \
    "cannot resolve base ref" "$out_3ljk"
is   "landing trigger fires for gitea-remote repo" 0 "$rc_3ljk"
want "bd create called for gitea-remote repo" "create" "$(cat "$BD_LOG")"

echo
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
