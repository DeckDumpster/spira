#!/usr/bin/env bash
#
# test-pr-notify.sh — pr-notify.sh finds green/unmerged and red PRs, and spira/* branches
# with no PR, using only the watcher log and a stubbed gh binary.
#
#   ./test-pr-notify.sh
#
# WHAT IT HOLDS
#
#   1. POSITIVE CONTROL FIRST. A stub gh that returns one green PR and one red PR proves the
#      matcher fires before any absence assertion is trusted.
#
#   2. ACTIONABLE KEYWORDS. Green PRs emit lines containing ⚠; red PRs emit lines containing
#      FAIL. Both are in the default SPIRA_ACTIONABLE set, so watchd notify picks them up.
#
#   3. LOG APPEND MODE. Running the script without --show writes to the watcher log, not
#      stdout. A second run appends; the cursor is not advanced.
#
#   4. SHOW MODE. --show prints to stdout and does not touch the log.
#
#   5. BRANCHES WITH NO PR. A spira/* branch ahead of base with no open PR emits ⚠ BRANCH.
#      A branch with an open PR emits nothing.
#
#   6. PENDING AND NO-CHECK PRs ARE SKIPPED. A PR whose checks have not completed yet is not
#      green and not red; it appears in neither list.
#
#   7. SILENT WHEN NOTHING TO REPORT. No managed repos, or all PRs pending: no output.
#
#   8. PUSH AND HOLD REPOS SKIPPED. Only pr-mode repos are scanned.
#
#   9. UNIT RENDERING. The service and timer are rendered, fenced, and enabled.
#
# ISOLATION. pr-notify.sh is run from the real $HERE directory with SPIRA_CONF pointing at
# a scratch config, SPIRA_REPO_MAP pointing at a scratch repo-map, and gh injected via
# SPIRA_PATH — so the operator's own configuration and repositories cannot influence any
# verdict (law-gates-run-in-a-clean-environment). SPIRA_REPO is set to a scratch directory
# so the harness-repo branch check never touches a real checkout.

# covers: spira/pr-notify.sh spira/watchers systemd/spira-pr-notify.service systemd/spira-pr-notify.timer systemd/units.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"

pass=0; fail=0
ok()   { printf '  ok    %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  FAIL  %s\n        got: %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
is()   { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "want [$2] got [$3]"; fi; }
has()  { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "[$2] missing [$3]"; esac; }
hasnt(){ case "$2" in *"$3"*) bad "$1" "[$2] should not contain [$3]" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home" "$TMP/run"

# ---- Fixture: scratch config ------------------------------------------------------------
# ALL VALUES PINNED TO NON-DEFAULTS (law-gates-run-in-a-clean-environment).
CONF="$TMP/spira.conf"
RUN="$TMP/run"
printf 'SPIRA_RUN = %s\n' "$RUN" > "$CONF"

# ---- Fixture: a fake git repo for a managed pr-mode repo --------------------------------
FAKE_REPO="$TMP/fake-managed"
git init --quiet -b main "$FAKE_REPO" 2>/dev/null \
    || { git init --quiet "$FAKE_REPO" && git -C "$FAKE_REPO" checkout -qb main 2>/dev/null; } \
    || git init --quiet "$FAKE_REPO"
git -C "$FAKE_REPO" config user.email "test@example.com"
git -C "$FAKE_REPO" config user.name  "Test"
printf 'content\n' > "$FAKE_REPO/file"
git -C "$FAKE_REPO" add file
git -C "$FAKE_REPO" commit -q -m "base"
git -C "$FAKE_REPO" remote add origin "https://github.com/example/managed.git" 2>/dev/null || true

# ---- Fixture: a fake git repo for spira/* branch checks --------------------------------
BRANCH_REPO="$TMP/branch-repo"
git init --quiet -b main "$BRANCH_REPO" 2>/dev/null \
    || { git init --quiet "$BRANCH_REPO" && git -C "$BRANCH_REPO" checkout -qb main 2>/dev/null; }
git -C "$BRANCH_REPO" config user.email "test@example.com"
git -C "$BRANCH_REPO" config user.name  "Test"
printf 'base\n' > "$BRANCH_REPO/readme"
git -C "$BRANCH_REPO" add readme
git -C "$BRANCH_REPO" commit -q -m "base"
# Make a bare remote and push main so `origin/main` exists.
BARE="$TMP/bare-remote"
git clone --quiet --bare "$BRANCH_REPO" "$BARE"
git -C "$BRANCH_REPO" remote add origin "$BARE"
git -C "$BRANCH_REPO" fetch --quiet origin
# Create a spira/* branch ahead of main and push to the remote.
git -C "$BRANCH_REPO" checkout -q -b spira/test-bead
printf 'aeon work\n' > "$BRANCH_REPO/work"
git -C "$BRANCH_REPO" add work
git -C "$BRANCH_REPO" commit -q -m "spira/test-bead: work"
git -C "$BRANCH_REPO" push -q origin spira/test-bead
git -C "$BRANCH_REPO" checkout -q main
# Make sure origin/main and spira/test-bead are both visible in remote refs.
git -C "$BRANCH_REPO" fetch --quiet origin

# ---- Fixture: repo-map ------------------------------------------------------------------
REPO_MAP="$TMP/repo-map"
printf '%s|%s|pr\n'   "managed"  "$FAKE_REPO"   >> "$REPO_MAP"
printf '%s|%s|push\n' "harness"  "$BRANCH_REPO" >> "$REPO_MAP"

# ---- Stub: gh ---------------------------------------------------------------------------
# Injected via SPIRA_PATH so pr-notify.sh (which loads lib.sh → conf.sh → sets PATH) finds
# the stub first. GH_PR_RESPONSE and GH_BRANCH_HAS_PR are passed as explicit env vars.
GH_LOG="$TMP/gh.log"
GH_BIN="$TMP/gh-bin"
mkdir -p "$GH_BIN"
cat > "$GH_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$*" in
    *"--head "*)
        echo "${GH_BRANCH_HAS_PR:-0}" ;;
    *"--state open --json"*)
        echo "${GH_PR_RESPONSE:-[]}" ;;
    *) echo "[]" ;;
esac
GHEOF
chmod +x "$GH_BIN/gh"

# run <args...> -> pr-notify.sh in a clean env; stdout to $TMP/out, stderr to $TMP/err.
# GH_PR_RESPONSE, GH_BRANCH_HAS_PR, and GH_LOG are passed through explicitly.
# SPIRA_PATH carries the stub gh so conf.sh's PATH rebuild keeps it at the front.
run() {
    env -i HOME="$TMP/home" PATH="$PATH" \
        SPIRA_PATH="$GH_BIN" \
        SPIRA_CONF="$CONF" \
        SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$TMP/empty-repo" \
        GH_LOG="$GH_LOG" \
        GH_PR_RESPONSE="${GH_PR_RESPONSE:-[]}" \
        GH_BRANCH_HAS_PR="${GH_BRANCH_HAS_PR:-0}" \
        bash "$HERE/pr-notify.sh" "$@" \
        > "$TMP/out" 2> "$TMP/err"
}

echo "test-pr-notify.sh"

# ====================================================================================
# 1. POSITIVE CONTROL — the matcher can find green and red PRs before any absence
#    assertion is trusted.
# ====================================================================================
echo
echo "positive control: green and red PRs are found"

GH_PR_RESPONSE='[
  {"number":10,"title":"Green PR","headRefName":"feature/a","statusCheckRollup":[
    {"status":"COMPLETED","conclusion":"SUCCESS","name":"ci"}]},
  {"number":11,"title":"Red PR","headRefName":"feature/b","statusCheckRollup":[
    {"status":"COMPLETED","conclusion":"FAILURE","name":"ci"}]}
]'
GH_BRANCH_HAS_PR=0
: > "$GH_LOG"
run --show
out="$(cat "$TMP/out")"
has "green PR emits ⚠"      "$out" "⚠"
has "green PR number"        "$out" "#10"
has "green PR title"         "$out" "Green PR"
has "red PR emits FAIL"      "$out" "FAIL"
has "red PR number"          "$out" "#11"
has "red PR title"           "$out" "Red PR"
has "repo label is present"  "$out" "[managed]"
is  "stderr is clean"        ""     "$(cat "$TMP/err")"

# ====================================================================================
# 2. ACTIONABLE KEYWORDS — ⚠ for green, FAIL for red.
# ====================================================================================
echo
echo "actionable keywords"
has "green uses ⚠ (in SPIRA_ACTIONABLE)"  "$out" "⚠ GREEN #10"
has "red uses FAIL (in SPIRA_ACTIONABLE)" "$out" "FAIL RED #11"
hasnt "green does not use FAIL"           "$out" "FAIL GREEN"
hasnt "red does not use ⚠ GREEN"          "$out" "⚠ GREEN #11"

# ====================================================================================
# 3. LOG APPEND MODE — without --show, writes to the watcher log and is silent on stdout.
# ====================================================================================
echo
echo "log append mode"

GH_PR_RESPONSE='[{"number":20,"title":"Appended PR","headRefName":"f","statusCheckRollup":[
  {"status":"COMPLETED","conclusion":"SUCCESS","name":"ci"}]}]'
GH_BRANCH_HAS_PR=0
LOGFILE="$RUN/watchd/pr-notify.log"
rm -f "$LOGFILE"
run; rc=$?
is "script exits 0"                      "0"  "$rc"
is "stdout is empty in log mode"         ""   "$(cat "$TMP/out")"
is "log file was created"                "yes" "$([ -f "$LOGFILE" ] && echo yes || echo no)"
has "log contains the finding"           "$(cat "$LOGFILE" 2>/dev/null)" "⚠ GREEN #20"

# A SECOND RUN APPENDS.
GH_PR_RESPONSE='[{"number":21,"title":"Second PR","headRefName":"g","statusCheckRollup":[
  {"status":"COMPLETED","conclusion":"SUCCESS","name":"ci"}]}]'
run
lines="$(wc -l < "$LOGFILE")"
is "second run appends (2 lines)"        "2" "$(echo "$lines" | tr -d ' ')"
has "first run still present"            "$(cat "$LOGFILE")" "#20"
has "second run appended"                "$(cat "$LOGFILE")" "#21"

# ====================================================================================
# 4. SHOW MODE — prints to stdout, log unchanged.
# ====================================================================================
echo
echo "--show mode"

GH_PR_RESPONSE='[{"number":30,"title":"Show PR","headRefName":"h","statusCheckRollup":[
  {"status":"COMPLETED","conclusion":"FAILURE","name":"ci"}]}]'
orig_size="$(wc -l < "$LOGFILE" | tr -d ' ')"
run --show
is "stdout has finding"    "yes" "$(grep -qF 'FAIL RED #30' "$TMP/out" && echo yes || echo no)"
is "log unchanged"         "$orig_size" "$(wc -l < "$LOGFILE" | tr -d ' ')"

# ====================================================================================
# 5. BRANCHES WITH NO PR — requires a pr-mode repo with a real git remote.
# ====================================================================================
echo
echo "branches with no PR"

# BRANCH_REPO is push-mode in the default map; add it as pr-mode with a base of main.
printf '%s|%s|pr|||\n' "branchrepo" "$BRANCH_REPO" >> "$REPO_MAP"

GH_PR_RESPONSE='[]'
GH_BRANCH_HAS_PR=0
run --show
out="$(cat "$TMP/out")"
has "spira/* branch with no PR emits ⚠ BRANCH" "$out" "⚠ BRANCH spira/test-bead"
has "carries repo label"                         "$out" "[branchrepo]"

# A BRANCH WITH AN OPEN PR IS NOT REPORTED.
GH_BRANCH_HAS_PR=1
run --show
hasnt "branch with a PR is not reported" "$(cat "$TMP/out")" "⚠ BRANCH spira/test-bead"
GH_BRANCH_HAS_PR=0

# Restore repo-map without branch repo for remaining tests.
printf '%s|%s|pr\n'   "managed"  "$FAKE_REPO" > "$REPO_MAP"
printf '%s|%s|push\n' "harness"  "$BRANCH_REPO" >> "$REPO_MAP"

# ====================================================================================
# 6. PENDING AND NO-CHECK PRs ARE SKIPPED.
# ====================================================================================
echo
echo "pending and no-check PRs are skipped"

GH_PR_RESPONSE='[
  {"number":40,"title":"Pending PR","headRefName":"p","statusCheckRollup":[
    {"status":"IN_PROGRESS","conclusion":null,"name":"ci"}]},
  {"number":41,"title":"No-check PR","headRefName":"q","statusCheckRollup":null},
  {"number":42,"title":"Empty-check PR","headRefName":"r","statusCheckRollup":[]}
]'
run --show
out="$(cat "$TMP/out")"
hasnt "pending PR is not green or red" "$out" "#40"
hasnt "null-check PR is skipped"       "$out" "#41"
hasnt "empty-check PR is skipped"      "$out" "#42"
is "nothing emitted for pending"       "" "$out"

# ====================================================================================
# 7. SILENT WHEN NOTHING TO REPORT.
# ====================================================================================
echo
echo "silent when nothing to report"

GH_PR_RESPONSE='[]'
run --show
is "empty PR list → no output" "" "$(cat "$TMP/out")"
is "stderr is clean"           "" "$(cat "$TMP/err")"

# ====================================================================================
# 8. PUSH AND HOLD REPOS SKIPPED.
# ====================================================================================
echo
echo "push and hold repos are not scanned"

# harness is push-mode in the repo-map. Add a hold-mode too.
printf '%s|%s|hold\n' "held" "$FAKE_REPO" >> "$REPO_MAP"
: > "$GH_LOG"
GH_PR_RESPONSE='[]'
run --show
calls="$(cat "$GH_LOG")"
# gh is called for FAKE_REPO (managed, pr-mode) but not for BRANCH_REPO (harness, push-mode)
# or the held repo. Since the stub doesn't distinguish callers, we verify no findings
# mention the push or hold repo names.
hasnt "push-mode repo name not in findings" "$(cat "$TMP/out")" "[harness]"
hasnt "hold-mode repo not in findings"      "$(cat "$TMP/out")" "[held]"

# Restore repo-map.
printf '%s|%s|pr\n'   "managed"  "$FAKE_REPO" > "$REPO_MAP"
printf '%s|%s|push\n' "harness"  "$BRANCH_REPO" >> "$REPO_MAP"

# ====================================================================================
# 9. UNIT RENDERING — the service and timer are rendered, fenced, and enabled.
# ====================================================================================
echo
echo "unit rendering"

RCONF="$TMP/render.conf"
printf 'SPIRA_RUN = %s\nSPIRA_PROD = \n' "$RUN" > "$RCONF"

rendered="$(env -i HOME="$TMP/home" PATH="$PATH" SPIRA_CONF="$RCONF" \
    bash "$ROOT/systemd/install.sh" --render 2>/dev/null)"
is "renderer produced units" "yes" "$([ -n "$rendered" ] && echo yes || echo no)"

svc="$(awk '/^===== spira-pr-notify-prod.service =====$/{f=1;next} /^===== /{f=0} f' <<< "$rendered")"
tmr="$(awk '/^===== spira-pr-notify-prod.timer =====$/{f=1;next} /^===== /{f=0} f' <<< "$rendered")"
is "service is rendered"    "yes" "$([ -n "$svc" ] && echo yes || echo no)"
is "timer is rendered"      "yes" "$([ -n "$tmr" ] && echo yes || echo no)"

has "service runs pr-notify.sh"      "$svc" "pr-notify.sh"
has "service is CPU-fenced"          "$svc" "CPUQuota="
has "service is niced"               "$svc" "Nice="
has "service appends to watcher log" "$svc" "/watchd/pr-notify.log"
has "timer fires every 30 minutes"   "$tmr" "OnUnitActiveSec=30min"
hasnt "no placeholder in service"    "$svc" "@"
hasnt "no placeholder in timer"      "$tmr" "@"

STUB="$TMP/stub"; mkdir -p "$STUB"
cat > "$STUB/systemctl" <<EOF
#!/usr/bin/env bash
mkdir -p "$TMP/active"
case "\$*" in
    *"is-active"*)
        _u="\${*##* }"
        [ -f "$TMP/active/\$_u" ] && printf 'active\n' || printf 'inactive\n' ;;
    *"list-"*) : ;;
    *)
        printf '%s\n' "\$*" >> "$TMP/systemctl.log"
        case "\$*" in
            *"enable --now "*) touch "$TMP/active/\${*##*enable --now }" ;;
            *"restart "*)      touch "$TMP/active/\${*##*restart }" ;;
        esac ;;
esac
exit 0
EOF
printf '#!/usr/bin/env bash\nexit 0\n' > "$STUB/loginctl"
chmod +x "$STUB/systemctl" "$STUB/loginctl"
IHOME="$TMP/ihome"; mkdir -p "$IHOME"
: > "$TMP/systemctl.log"
printf 'SPIRA_RUN = %s\nSPIRA_COCKPIT = %s\nSPIRA_WATCHERS = %s\nSPIRA_PATH = %s\nSPIRA_PROD = %s\n' \
    "$RUN" "$ROOT/cockpit" "$HERE/watchers" "$STUB" "$HERE" > "$TMP/install.conf"
env -i HOME="$IHOME" PATH="$STUB:$PATH" SPIRA_CONF="$TMP/install.conf" \
    SPIRA_INSTALL_FORCE=1 SPIRA_HOME="$HERE" \
    bash "$ROOT/systemd/install.sh" > "$TMP/install.out" 2>&1
log="$(cat "$TMP/systemctl.log")"
has "install ran"                        "$log" "daemon-reload"
has "timer is enabled on install"        "$log" "enable --now spira-pr-notify-prod.timer"
hasnt "service not separately enabled"   "$log" "enable --now spira-pr-notify-prod.service"
has "timer unit file is installed" \
    "$(ls "$IHOME/.config/systemd/user" 2>/dev/null)" "spira-pr-notify-prod.timer"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
