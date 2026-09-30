#!/usr/bin/env bash
#
# test-pr-notify.sh — pr-notify.sh's classifier over its own JSON seam, plus the transition
# engine that covers every repo-map land mode (push, pr, queue, hold) and reports opened,
# RED, GREEN, merged and closed exactly once each.
#
# WHAT IT HOLDS
#
#   1. POSITIVE CONTROL FIRST. Pending, green and red PRs in the same JSON prove the
#      classifier fires — including naming the failing checks on a red one — before any
#      absence assertion is trusted.
#   2. SILENT ON UNPARSEABLE/EMPTY INPUT.
#   3. QUEUE-MODE IS COVERED (the acceptance case, and the one that fails on the code this
#      replaced): a fixture repo-map row with land=queue goes red, and pr-notify reports it.
#      So do push and hold rows, alongside a pr row — "whatever its land mode".
#   4. TRANSITIONS, NOT RE-LISTING. A second tick over an unchanged PR emits nothing; a third
#      tick where its checks flip emits exactly one new line. GREEN reads the same whatever the
#      land mode — auto-merge is already armed under `pr`, so nobody needs to hand-merge it
#      either (law-green-prs-merge-themselves).
#   5. MERGED AND CLOSED are reported once, from `gh pr view`, when a tracked PR leaves the
#      open list — and a gh call that fails leaves the tracked PR in place for a retry rather
#      than guessing it away.
#   6. BRANCHES WITH NO PR (T2: real git, stubbed gh) — pr-mode only; unchanged from before.
#   7. THE MANIFEST ROW is a daemon now, not a log fed by an external timer — this is what
#      lets `watchd status` show it active instead of external.
#   8. EVERY TRANSITION IS ALSO MAILED to the concierge mailbox directly (`--kind event`),
#      not only logged — delivery this way does not depend on a session holding an
#      in-session Monitor or on the watch-notify escalation timer.
#
# THE CLASSIFIER (_PR_STATUS_PY) IS A PURE FUNCTION OF STDIN JSON, exercised directly; the
# transition engine is exercised through the real script with a stubbed `gh` so a PR's
# lifecycle (open -> checks resolve -> merged) is driven exactly as production drives it,
# never a hand-written model of what `gh` would say.
#
# tier: T1
# covers: spira/pr-notify.sh spira/watchers spira/mail.sh spira/mail/kinds UC-operator-channel-38
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1" "$2" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1" "$2" ;; *) ok "$1" ;; esac; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/home" "$TMP/run"

# conf.sh's spira.toml auto-convert shells out to spira-config (sp-zs04v.2), by name on the
# suite's PATH (sp-gypjk); without it every fixture SPIRA_RUN silently reverts to its
# computed default and the mail assertions below check a directory pr-notify never wrote to.
command -v spira-config >/dev/null 2>&1 || bail "spira-config is not on PATH"

# ALL VALUES PINNED TO NON-DEFAULTS so sourcing pr-notify.sh cannot read the operator's own
# configuration (law-gates-run-in-a-clean-environment).
CONF="$TMP/spira.conf"
RUN="$TMP/run"
printf 'SPIRA_ID_PREFIX = sp\nSPIRA_RUN = %s\n' "$RUN" > "$CONF"
# REPO_MAP PINNED (empty for now) BEFORE THE FIRST SOURCE BELOW: an unset SPIRA_REPO_MAP
# falls back to this checkout's own real repo-map, auto-converting it into this fixture's
# spira.toml — and every later, smaller fixture conversion then trips spira-config convert's
# shrink guard, so $CONF's SPIRA_RUN is silently never read.
REPO_MAP="$TMP/repo-map"
: > "$REPO_MAP"
export SPIRA_CONF="$CONF" HOME="$TMP/home" SPIRA_REPO_MAP="$REPO_MAP"

# SOURCEABLE, AND SILENT WHEN IT IS (pr-notify.sh's own guard): this reaches _PR_STATUS_PY
# without triggering a live repo-map scan.
# shellcheck disable=SC1090
. "$HERE/pr-notify.sh" ""

classify() { python3 -c "$_PR_STATUS_PY" ; }   # classify — JSON on stdin

echo "test-pr-notify.sh"

# ====================================================================================
# 1. POSITIVE CONTROL — pending, green and red are all found, and a red PR names its
#    failing checks.
# ====================================================================================
echo
echo "positive control: pending, green and red are all classified"

out="$(classify <<'JSON'
[
  {"number":10,"title":"Pending PR","headRefName":"a","statusCheckRollup":[
    {"status":"IN_PROGRESS","conclusion":null,"name":"ci"}]},
  {"number":11,"title":"Green PR","headRefName":"b","statusCheckRollup":[
    {"status":"COMPLETED","conclusion":"SUCCESS","name":"ci"}]},
  {"number":12,"title":"Red PR","headRefName":"c","statusCheckRollup":[
    {"status":"COMPLETED","conclusion":"FAILURE","name":"suites"},
    {"status":"COMPLETED","conclusion":"SUCCESS","name":"lint"}]}
]
JSON
)"
has "pending PR classified pending"        "$out" $'10\tpending\tPending PR'
has "green PR classified green"            "$out" $'11\tgreen\tGreen PR'
has "red PR classified red"                "$out" $'12\tred\tRed PR'
has "red PR names its failing check"       "$out" "suites"
hasnt "red PR does not name its passing check as failing" "$out" $'\tsuites,lint'

out="$(classify <<'JSON'
[{"number":20,"title":"No checks yet","headRefName":"d","statusCheckRollup":[]}]
JSON
)"
is "empty statusCheckRollup classifies pending, not skipped" $'20\tpending\tNo checks yet\t' "$out"

# ====================================================================================
# 2. SILENT ON UNPARSEABLE/EMPTY INPUT.
# ====================================================================================
echo
echo "silent on unparseable or empty input"
is "empty PR list -> no output"       "" "$(classify <<< '[]')"
is "unparseable JSON -> no output, no crash" "" "$(classify <<< 'not json')"

# ====================================================================================
# 3-5. THE TRANSITION ENGINE, run as the real script with a stubbed gh — one repo per land
#    mode, all fed through the one repo-map. GH_PLAN/<repo>.json is `gh pr list`'s answer for
#    that repo; GH_STATE/<repo>.txt is `gh pr view`'s answer once a PR drops out of the list.
# ====================================================================================
echo
echo "transitions across every land mode (push, pr, queue, hold)"

mkdir -p "$TMP/repos/queue-repo/.git" "$TMP/repos/pr-repo/.git" \
         "$TMP/repos/push-repo/.git" "$TMP/repos/hold-repo/.git"
{
    printf '%s|%s|queue\n' "queue-repo" "$TMP/repos/queue-repo"
    printf '%s|%s|pr\n'    "pr-repo"    "$TMP/repos/pr-repo"
    printf '%s|%s|push\n'  "push-repo"  "$TMP/repos/push-repo"
    printf '%s|%s|hold\n'  "hold-repo"  "$TMP/repos/hold-repo"
} > "$REPO_MAP"

GH_LOG="$TMP/gh.log"
GH_BIN="$TMP/gh-bin"; mkdir -p "$GH_BIN"
cat > "$GH_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$1 $2" in
    "pr list")
        case "$*" in
            *"--head "*) printf '%s\n' "${GH_BRANCH_HAS_PR:-0}" ;;
            *) cat "$(pwd)/.gh-pr-list.json" 2>/dev/null || printf '[]\n' ;;
        esac
        ;;
    "pr view")
        n="$3"
        awk -v n="$n" '$1==n{print $2; exit}' "$(pwd)/.gh-pr-state" 2>/dev/null
        ;;
    *) printf '[]\n' ;;
esac
GHEOF
chmod +x "$GH_BIN/gh"

run() {  # run [args...] -> pr-notify.sh in a clean env; stdout in $TMP/out
    env -i HOME="$TMP/home" PATH="$GH_BIN:$PATH" \
        SPIRA_CONF="$CONF" SPIRA_REPO_MAP="$REPO_MAP" \
        SPIRA_REPO="$TMP/empty-repo" GH_LOG="$GH_LOG" \
        pr-notify.sh "$@" > "$TMP/out" 2>"$TMP/err"
}

pending_json() {  # pending_json <num> <title>
    printf '[{"number":%s,"title":"%s","headRefName":"x","statusCheckRollup":[{"status":"IN_PROGRESS","conclusion":null,"name":"ci"}]}]' "$1" "$2"
}
red_json() {      # red_json <num> <title> <check-name>
    printf '[{"number":%s,"title":"%s","headRefName":"x","statusCheckRollup":[{"status":"COMPLETED","conclusion":"FAILURE","name":"%s"}]}]' "$1" "$2" "$3"
}
green_json() {    # green_json <num> <title>
    printf '[{"number":%s,"title":"%s","headRefName":"x","statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS","name":"ci"}]}]' "$1" "$2"
}

for r in queue-repo pr-repo push-repo hold-repo; do
    pending_json 1 "Round batch" > "$TMP/repos/$r/.gh-pr-list.json"
done

# --- TICK 1: every repo's PR is freshly open and still pending -----------------------
run --show
out1="$(cat "$TMP/out")"
for r in queue-repo pr-repo push-repo hold-repo; do
    has   "tick1: $r's PR is reported OPENED"        "$out1" "OPENED #1 Round batch [$r]"
    hasnt "tick1: $r has no RED yet (still pending)" "$out1" "FAIL RED"
done
# OPENED is mailed too (every transition is); clear it so TICK 3's count below isolates the
# RED transitions it actually asserts on.
rm -f "$RUN"/mail/concierge/new/*

# --- TICK 2: unchanged — nothing re-reported (transitions, not state) ---------------
run --show
is "tick2: an unchanged PR emits nothing" "" "$(cat "$TMP/out")"

# --- TICK 3: every repo's PR goes red — THE ACCEPTANCE CASE. This is what the old
#     land=pr-only filter made invisible for queue-repo (and push/hold): the code being
#     replaced skipped every one of these rows outright. -----------------------------
for r in queue-repo pr-repo push-repo hold-repo; do
    red_json 1 "Round batch" suites > "$TMP/repos/$r/.gh-pr-list.json"
done
run --show
out3="$(cat "$TMP/out")"
for r in queue-repo pr-repo push-repo hold-repo; do
    has "tick3: $r's now-red PR is reported, naming the failing check" \
        "$out3" "FAIL RED #1 Round batch [$r]: suites"
done

# --- TICK 3 MAIL: THE SCOPE ADDITION — every transition is also mailed straight to the
#     concierge mailbox, so delivery does not depend on a session holding an in-session
#     Monitor or on the separate (and separately broken) watch-notify escalation timer.
#     This is what makes queue-repo's red batch PR reach an unread message with no
#     Concierge session attached to anything.
mail_matches="$(grep -l "FAIL RED #1 Round batch \[queue-repo\]: suites" \
    "$RUN"/mail/concierge/new/* 2>/dev/null | wc -l | tr -d ' ')"
is "tick3: queue-repo's RED is mailed to the concierge mailbox" "1" "${mail_matches:-0}"
mail_unread="$(ls "$RUN/mail/concierge/new" 2>/dev/null | wc -l | tr -d ' ')"
is "tick3: one mail per repo's RED transition (4 repos)" "4" "$mail_unread"
rm -f "$RUN"/mail/concierge/new/*

# --- TICK 4: unchanged again — the RED is not re-announced --------------------------
run --show
is "tick4: an unchanged RED emits nothing" "" "$(cat "$TMP/out")"

# --- TICK 5: GREEN reads differently under land=pr than everywhere else -------------
for r in queue-repo pr-repo push-repo hold-repo; do
    green_json 1 "Round batch" > "$TMP/repos/$r/.gh-pr-list.json"
done
run --show
out5="$(cat "$TMP/out")"
has   "tick5: pr-repo green lands on its own (auto-merge is already armed)" \
      "$out5" "GREEN #1 Round batch [pr-repo]: lands automatically"
has   "tick5: queue-repo green lands on its own" "$out5" "GREEN #1 Round batch [queue-repo]: lands automatically"
hasnt "tick5: no green is flagged ⚠ (a hand-merge there fights auto-merge, law-green-prs-merge-themselves)" \
      "$out5" "⚠ GREEN"

# --- TICK 6: the PR merges — leaves the open list, gh pr view says MERGED ------------
for r in queue-repo pr-repo push-repo hold-repo; do
    printf '[]\n' > "$TMP/repos/$r/.gh-pr-list.json"
    printf '1 MERGED\n' > "$TMP/repos/$r/.gh-pr-state"
done
run --show
out6="$(cat "$TMP/out")"
for r in queue-repo pr-repo push-repo hold-repo; do
    has "tick6: $r's merge is reported once" "$out6" "MERGED #1 Round batch [$r]"
done

# --- TICK 7: nothing left to track — silent -----------------------------------------
run --show
is "tick7: nothing tracked, nothing reported" "" "$(cat "$TMP/out")"

# --- A NEW PR OPENS ALREADY RED — the exact shape of the reported incident: a daemon
#     that only just started (or a PR that resolved before the next tick) discovering a
#     batch that is already broken must still say so, not wait for a pending baseline. -----
red_json 2 "Round batch 2" suites > "$TMP/repos/queue-repo/.gh-pr-list.json"
rm -f "$TMP/repos/queue-repo/.gh-pr-state"
run --show
out8="$(cat "$TMP/out")"
has "a PR discovered already red gets OPENED and RED in the same tick" \
    "$out8" "OPENED #2 Round batch 2 [queue-repo]"
has "  ...and the RED line names the failing check" \
    "$out8" "FAIL RED #2 Round batch 2 [queue-repo]: suites"

# --- A gh FAILURE THIS TICK LEAVES STATE UNTOUCHED, retried next time ---------------
cat > "$GH_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
exit 1
GHEOF
chmod +x "$GH_BIN/gh"
run --show
is "a gh failure emits nothing (never a guess)" "" "$(cat "$TMP/out")"

# ====================================================================================
# 6. BRANCHES WITH NO PR (T2: real git, stubbed gh) — pr-mode only, unchanged behaviour.
# ====================================================================================
echo
echo "branches with no PR (T2: real git, stubbed gh)"

BRANCH_REPO="$TMP/branch-repo"
git init --quiet -b main "$BRANCH_REPO" 2>/dev/null \
    || { git init --quiet "$BRANCH_REPO" && git -C "$BRANCH_REPO" checkout -qb main 2>/dev/null; }
git -C "$BRANCH_REPO" config user.email "test@example.com"
git -C "$BRANCH_REPO" config user.name  "Test"
printf 'base\n' > "$BRANCH_REPO/readme"
git -C "$BRANCH_REPO" add readme
git -C "$BRANCH_REPO" commit -q -m "base"
BARE="$TMP/bare-remote"
git clone --quiet --bare "$BRANCH_REPO" "$BARE"
git -C "$BRANCH_REPO" remote add origin "$BARE"
git -C "$BRANCH_REPO" fetch --quiet origin
git -C "$BRANCH_REPO" checkout -q -b spira/test-bead
printf 'aeon work\n' > "$BRANCH_REPO/work"
git -C "$BRANCH_REPO" add work
git -C "$BRANCH_REPO" commit -q -m "spira/test-bead: work"
git -C "$BRANCH_REPO" push -q origin spira/test-bead
git -C "$BRANCH_REPO" checkout -q main
git -C "$BRANCH_REPO" fetch --quiet origin

BRANCH_MAP="$TMP/branch-repo-map"
printf '%s|%s|pr\n'   "branchrepo" "$BRANCH_REPO" >  "$BRANCH_MAP"
printf '%s|%s|push\n' "harness"    "$BRANCH_REPO" >> "$BRANCH_MAP"
printf '%s|%s|hold\n' "held"       "$BRANCH_REPO" >> "$BRANCH_MAP"

cat > "$GH_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$*" in
    *"--head "*)          printf '%s\n' "${GH_BRANCH_HAS_PR:-0}" ;;
    *"--state open --json"*) printf '[]\n' ;;
    *) printf '[]\n' ;;
esac
GHEOF
chmod +x "$GH_BIN/gh"

runb() {  # runb <args...> -> pr-notify.sh against BRANCH_MAP
    env -i HOME="$TMP/home" PATH="$GH_BIN:$PATH" \
        SPIRA_CONF="$CONF" SPIRA_REPO_MAP="$BRANCH_MAP" \
        SPIRA_REPO="$TMP/empty-repo" GH_LOG="$GH_LOG" \
        GH_BRANCH_HAS_PR="${GH_BRANCH_HAS_PR:-0}" \
        pr-notify.sh "$@" > "$TMP/out" 2>"$TMP/err"
}

GH_BRANCH_HAS_PR=0
runb --show
outb="$(cat "$TMP/out")"
has   "spira/* branch with no PR emits ⚠ BRANCH" "$outb" "⚠ BRANCH spira/test-bead"
has   "carries repo label"                        "$outb" "[branchrepo]"
hasnt "push-mode repo's branch check is not run"  "$outb" "BRANCH spira/test-bead: no PR [harness]"
hasnt "hold-mode repo's branch check is not run"  "$outb" "BRANCH spira/test-bead: no PR [held]"

GH_BRANCH_HAS_PR=1
runb --show
hasnt "branch with a PR is not reported" "$(cat "$TMP/out")" "⚠ BRANCH spira/test-bead"

# ====================================================================================
# 7. THE MANIFEST ROW IS A DAEMON, not a log fed by an external timer — this is what lets
#    `watchd status` show pr-notify active instead of external, and what puts it on the
#    same generic spira-watch@ rendering test-units-lint.sh already checks.
# ====================================================================================
echo
echo "the watchd manifest row"
manifest_line="$(grep '^pr-notify|' "$HERE/watchers")"
has   "pr-notify is a daemon row"                 "$manifest_line" "pr-notify|daemon|"
has   "its target is the watch loop"              "$manifest_line" "pr-notify.sh watch"
hasnt "it is no longer a log row fed by a timer"   "$manifest_line" "pr-notify|log|"

# ====================================================================================
# 8. A REPO DROPPED FROM THE REPO-MAP GETS NO EVENTS, EVER — sp-iagac. A repository handed
#    off elsewhere (ephemeral-ci -> prod/agent-swarm) has no row here any more; the scan is
#    already built from `repo_names`, so proving a mapped repo's first RED still fires beside
#    an unmapped one's silence is what would have caught the old backlog at the source.
# ====================================================================================
echo
echo "a repo not in the repo-map produces no events; a mapped repo's first RED still does"

# Section 6 above left $GH_BIN/gh as its own branch-test stub; restore the "read
# .gh-pr-list.json from cwd" stub sections 3-5 relied on.
cat > "$GH_BIN/gh" <<'GHEOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:-/dev/null}"
case "$1 $2" in
    "pr list")
        case "$*" in
            *"--head "*) printf '%s\n' "${GH_BRANCH_HAS_PR:-0}" ;;
            *) cat "$(pwd)/.gh-pr-list.json" 2>/dev/null || printf '[]\n' ;;
        esac
        ;;
    "pr view")
        n="$3"
        awk -v n="$n" '$1==n{print $2; exit}' "$(pwd)/.gh-pr-state" 2>/dev/null
        ;;
    *) printf '[]\n' ;;
esac
GHEOF
chmod +x "$GH_BIN/gh"

mkdir -p "$TMP/repos/gone-repo/.git"
red_json 9 "Orphaned PR" suites > "$TMP/repos/gone-repo/.gh-pr-list.json"
# gone-repo is a real, gated checkout that simply has no row in $REPO_MAP any more.
GONE_MAP="$TMP/gone-repo-map"
printf '%s|%s|queue\n' "queue-repo" "$TMP/repos/queue-repo" > "$GONE_MAP"
red_json 1 "Round batch" suites > "$TMP/repos/queue-repo/.gh-pr-list.json"
rm -f "$TMP/repos/queue-repo/.gh-pr-state"

run_gone() {
    env -i HOME="$TMP/home" PATH="$GH_BIN:$PATH" \
        SPIRA_CONF="$CONF" SPIRA_REPO_MAP="$GONE_MAP" \
        SPIRA_REPO="$TMP/empty-repo" GH_LOG="$GH_LOG" \
        pr-notify.sh --show > "$TMP/out" 2>"$TMP/err"
}
run_gone
outg="$(cat "$TMP/out")"
hasnt "an unmapped repo's own checkout is never scanned"     "$outg" "gone-repo"
has   "a mapped repo's first RED still is, beside it"        "$outg" "FAIL RED #1 Round batch [queue-repo]: suites"
rm -f "$RUN"/mail/concierge/new/*

# ====================================================================================
# 9. THE ACTIONABLE FILTER (`pr-notify.sh actionable FILE`) — the read-time cleanup for a
#    backlog the producer already wrote: drop a line whose repo the map no longer carries
#    (the log outlives the repo-map edit that orphaned it), drop a kind SPIRA_ACTIONABLE does
#    not mark worth a look, and collapse repeats of the exact same line to one — the same
#    bead's same RED reported every poll is one finding, not one per poll.
# ====================================================================================
echo
echo "the actionable filter: unmapped repos dropped, kinds narrowed, repeats collapsed"

FLOG="$TMP/fixture-pr-notify.log"
cat > "$FLOG" <<EOF
OPENED #1 Round batch [queue-repo]
FAIL RED #1 Round batch [queue-repo]: suites
FAIL RED #1 Round batch [queue-repo]: suites
FAIL RED #1 Round batch [queue-repo]: suites
FAIL RED #9 Orphaned PR [gone-repo]: suites
GREEN #1 Round batch [queue-repo]: lands automatically
EOF

actionable() {
    env -i HOME="$TMP/home" PATH="$PATH" \
        SPIRA_CONF="$CONF" SPIRA_REPO_MAP="$GONE_MAP" SPIRA_REPO="$TMP/empty-repo" \
        SPIRA_ACTIONABLE="${SPIRA_ACTIONABLE_OVERRIDE:-}" \
        pr-notify.sh actionable "$FLOG"
}

out9="$(SPIRA_ACTIONABLE_OVERRIDE='ANSWERED|COMMENTED|ESCALAT|STRANDED|POISON|DEGRADED|BLOCKED|UNREACHABLE|FAIL|ERROR|LANDED|⚠ BRANCH' actionable)"
line_count="$(printf '%s\n' "$out9" | grep -c . )"
is   "actionable filter: exactly one surviving line (dup RED collapsed)" "1" "$line_count"
has  "actionable filter: the mapped repo's RED survives"                "$out9" "FAIL RED #1 Round batch [queue-repo]: suites"
hasnt "actionable filter: the unmapped repo's RED is dropped"            "$out9" "gone-repo"
hasnt "actionable filter: OPENED is not actionable"                      "$out9" "OPENED"
hasnt "actionable filter: GREEN is not actionable (it lands itself)"     "$out9" "GREEN"

tl_summary
