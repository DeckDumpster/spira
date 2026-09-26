#!/usr/bin/env bash
#
# test-landing-basefail-fix.sh — a base-fix branch (external_ref=basefail:<repo>:<suite>)
# is gated before all other branches and certified while the base is still red.
#
# PROPERTIES UNDER TEST:
#
#   1. FIX-FIRST + PASS. Fix bead has external_ref=basefail:<repo>:<suite>. Gate returns
#      PASS for the fix branch and BASE_FAIL for ten others. Pass certifies the fix branch
#      first; others are held.
#
#   2. BASE_FAIL CERTIFICATION. Gate returns BASE_FAIL for all branches including the fix.
#      Fix branch output shows the failing suite as GREEN. Pass certifies the fix via the
#      branch-green-on-suite path.
#
#   3. POSITIVE CONTROL. No fix bead. All branches held by BASE_FAIL. None certified; no
#      fix-front log.
#
#   4. BUDGET EXEMPT. With tight budget, fix branch is gated anyway; ten others are not
#      certified. gate_fits' own budget-cut log is covered by test-landing-gate-wait.sh,
#      not here.
#
# defect: sp-lnprs
# covers: spira/landing.sh spira/lib.sh
# timeout: 180
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }
before() {
    local la lb
    la=$(printf '%s\n' "$4" | grep -n "$2" | head -1 | cut -d: -f1)
    lb=$(printf '%s\n' "$4" | grep -n "$3" | head -1 | cut -d: -f1)
    [ -n "$la" ] && [ -n "$lb" ] && [ "$la" -lt "$lb" ] \
        && ok "$1" \
        || bad "$1" "[$2] (line ${la:--}) not before [$3] (line ${lb:--})"
}

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-landing-basefail-fix
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up landing-basefail-fix || {
    printf 'SKIP test-landing-basefail-fix: no testdb available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
REPONAME=fixture-repo
FAILSUITE=test-failing.sh
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"

cp "$HERE"/*.sh "$HERE"/*.py "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub confine.sh 'exit 0'
stub mail.sh '[ "${1:-}" = send ] || exit 0'
stub gh 'exit 1'
stub queue.sh 'exit 0'

cat > "$SH/repo-map" <<MAP
$REPONAME | $REPO | queue | |
MAP

# gate_stub_pass_for_fix: PASS for spira/sp-fix, BASE_FAIL for all others.
gate_stub_pass_for_fix() {
    cat > "$SH/gate.sh" <<GATESCRIPT
#!/usr/bin/env bash
BR="\$1"; NM="\${2:-?}"
if [ "\$BR" = "spira/sp-fix" ]; then
    printf 'gate: VERDICT=PASS reason=stub branch=%s repo=%s suite=-\n' "\$BR" "\$NM" >&2
    exit 0
fi
printf '%s\n' \\
    "gate: \$NM's own gate fails against origin/main — this branch did not cause it." \\
    "gate: command: testenv" \\
    "gate: red on origin/main: $FAILSUITE" \\
    "--- origin/main's own output ---" \\
    "$FAILSUITE RED 3.1s" \\
    "--- this branch's output ---" \\
    "$FAILSUITE RED 1.8s" \\
    "gate: fix the repository, or clear that command from the repo-map." >&2
printf 'gate: VERDICT=BASE_FAIL reason=base-red branch=%s repo=%s suite=$FAILSUITE\n' "\$BR" "\$NM" >&2
exit 76
GATESCRIPT
    chmod +x "$SH/gate.sh"
}

# gate_stub_basefail_green_fix: BASE_FAIL for all. Fix branch output shows FAILSUITE GREEN.
gate_stub_basefail_green_fix() {
    cat > "$SH/gate.sh" <<GATESCRIPT
#!/usr/bin/env bash
BR="\$1"; NM="\${2:-?}"
if [ "\$BR" = "spira/sp-fix" ]; then
    printf '%s\n' \\
        "gate: \$NM's own gate fails against origin/main — this branch did not cause it." \\
        "gate: command: testenv" \\
        "gate: red on origin/main: $FAILSUITE" \\
        "--- origin/main's own output ---" \\
        "$FAILSUITE RED 3.1s" \\
        "--- this branch's output ---" \\
        "test-other.sh GREEN 1.2s" \\
        "gate: fix the repository, or clear that command from the repo-map." >&2
    printf 'gate: VERDICT=BASE_FAIL reason=base-red branch=%s repo=%s suite=$FAILSUITE\n' "\$BR" "\$NM" >&2
    exit 76
fi
printf '%s\n' \\
    "gate: \$NM's own gate fails against origin/main — this branch did not cause it." \\
    "gate: command: testenv" \\
    "gate: red on origin/main: $FAILSUITE" \\
    "--- origin/main's own output ---" \\
    "$FAILSUITE RED 3.1s" \\
    "--- this branch's output ---" \\
    "$FAILSUITE RED 1.8s" \\
    "gate: fix the repository, or clear that command from the repo-map." >&2
printf 'gate: VERDICT=BASE_FAIL reason=base-red branch=%s repo=%s suite=$FAILSUITE\n' "\$BR" "\$NM" >&2
exit 76
GATESCRIPT
    chmod +x "$SH/gate.sh"
}

landing() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
        bash "$SH/landing.sh" 2>&1
}

landing_tight() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_BD="${SPIRA_BD:-$TESTDB_BD}" \
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO="$REPONAME" SPIRA_ID_PREFIX=sp \
    SPIRA_REPO_MAP="$SH/repo-map" SPIRA_GH="$SH/gh" \
    SPIRA_LAND_MAXSEC=1 SPIRA_LAND_GATE_RESERVE=2 \
        bash "$SH/landing.sh" 2>&1
}

# branch_fix <id> — closed fix bead with external_ref=basefail:<repo>:<suite>
branch_fix() {
    local id="$1"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "fix: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":1,"external_ref":"basefail:%s:%s","labels":[],"updated_at":"2026-09-21T20:00:00Z","closed_at":"2026-09-21T20:00:00Z","dependencies":[]}\n' \
        "$id" "$id" "$REPONAME" "$FAILSUITE" | testdb_seed
}

# branch_other <id> <priority> <closed_at> — a normal closed bead
branch_other() {
    local id="$1" pri="$2" cat="$3"
    git -C "$REPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    printf '%s\n' "$id" > "$RUN/worktree/$id/$id.txt"
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "feat: $id"
    printf '{"id":"%s","title":"%s","status":"closed","issue_type":"task","priority":%d,"labels":[],"updated_at":"%s","closed_at":"%s","dependencies":[]}\n' \
        "$id" "$id" "$pri" "$cat" "$cat" | testdb_seed
}

drop_branch() {
    local id="$1"
    git -C "$REPO" worktree remove --force "$RUN/worktree/$id" >/dev/null 2>&1 || true
    git -C "$REPO" branch -D "spira/$id" >/dev/null 2>&1 || true
}

seed_base() {
    testdb_reset
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-01T00:00:00Z"}
JSONL
}

drop_others() { for i in $(seq -w 0 9); do drop_branch "sp-other-$i"; done; }

echo "test-landing-basefail-fix.sh"

# -------------------------------------------------------------------
# TEST 1: FIX-FIRST + PASS PATH
# Gate: PASS for sp-fix, BASE_FAIL for all others.
# Fix must be certified; others held.
# -------------------------------------------------------------------
echo
echo "fix-first with cached pass:"
seed_base
gate_stub_pass_for_fix
rm -rf "$RUN/submitted"
branch_fix sp-fix
for i in $(seq -w 0 9); do branch_other "sp-other-$i" 2 "2026-09-21T19:0${i}:00Z"; done

out="$(landing)"

want  "fix branch certified"               "certified spira/sp-fix"       "$out"
want  "fix-front log line present"         "base-fix branch"              "$out"
want  "other branches held by BASE_FAIL"   "held — the base fails"        "$out"
before "fix certified before held message" "certified spira/sp-fix" "held — the base fails" "$out"
for i in $(seq -w 0 9); do
    nowant "sp-other-$i not certified" "certified spira/sp-other-$i" "$out"
done

drop_branch sp-fix; drop_others; rm -rf "$RUN/submitted"

# -------------------------------------------------------------------
# TEST 2: BASE_FAIL CERTIFICATION — branch output green on the suite
# Gate: BASE_FAIL for all. Fix branch section omits FAILSUITE (green).
# Fix must be certified via the branch-green path.
# -------------------------------------------------------------------
echo
echo "base-fail certification (branch green on suite):"
seed_base
gate_stub_basefail_green_fix
rm -rf "$RUN/submitted"
branch_fix sp-fix
for i in $(seq -w 0 9); do branch_other "sp-other-$i" 2 "2026-09-21T19:0${i}:00Z"; done

out="$(landing)"

want  "fix certified when branch green on suite"    "certified spira/sp-fix"                 "$out"
want  "certify log names the suite"                 "base-fix: spira/sp-fix is green on"     "$out"
want  "others held"                                 "held — the base fails"                  "$out"
for i in $(seq -w 0 9); do
    nowant "sp-other-$i not certified" "certified spira/sp-other-$i" "$out"
done

drop_branch sp-fix; drop_others; rm -rf "$RUN/submitted"

# -------------------------------------------------------------------
# TEST 3: POSITIVE CONTROL — no basefail-keyed bead
# Gate: BASE_FAIL for all. No fix bead → none certified, no fix-front log.
# -------------------------------------------------------------------
echo
echo "positive control (no basefail-keyed bead):"
seed_base
gate_stub_basefail_green_fix
rm -rf "$RUN/submitted"
for i in $(seq -w 0 9); do branch_other "sp-other-$i" 2 "2026-09-21T19:0${i}:00Z"; done

out="$(landing)"

nowant "no branch certified without fix bead" "certified"         "$out"
nowant "no fix-front log without fix bead"    "base-fix branch"   "$out"
want   "branches held by BASE_FAIL"           "held — the base fails" "$out"

drop_others; rm -rf "$RUN/submitted"

# -------------------------------------------------------------------
# TEST 4: BUDGET EXEMPT — fix branch gated despite exhausted budget
# With LAND_MAXSEC=1 RESERVE=2 (budget gone before any gate), the fix
# branch is still gated and certified; non-fix branches are deferred.
# -------------------------------------------------------------------
echo
echo "budget exempt for fix branch:"
seed_base
gate_stub_pass_for_fix
rm -rf "$RUN/submitted"
branch_fix sp-fix
for i in $(seq -w 0 9); do branch_other "sp-other-$i" 2 "2026-09-21T19:0${i}:00Z"; done

out="$(landing_tight)"

want "budget-exhaustion bypass logged"     "base-fix branch — gating despite budget exhaustion" "$out"
want "fix branch certified despite budget" "certified spira/sp-fix"                             "$out"
for i in $(seq -w 0 9); do
    nowant "sp-other-$i not certified under tight budget" "certified spira/sp-other-$i" "$out"
done

drop_branch sp-fix; drop_others; rm -rf "$RUN/submitted"

echo
printf 'results: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
