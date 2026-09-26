#!/usr/bin/env bash
#
# test-submitted-lands.sh — a work bead's close becomes open + spira-submitted (sp-qsona), and
#   in a NON-QUEUE repository that submitted bead must still land and close: aeon close ->
#   submitted -> landing pass lands it -> bead_close_on_land closes it.
#
# THE DEFECT THIS REPRODUCES. sp-qsona made bead_close_on_land the only thing that closes a
# work bead, called when the work reaches the base. Queue mode reaches it through the batch
# verdict. Every other mode reaches the base through landing.sh's CHECK 6 (push merges it,
# pr opens a pull request, hold gates it) — and CHECK 6 skipped, refused to certify and
# refused to land any branch whose bead was not `closed`. A submitted bead is open, so in
# push, pr and hold mode nothing ever landed and nothing ever closed. Release acceptance runs
# a push-mode scratch repo and failed phase A stage 5 ("no commit with bead id on origin/main
# after 121s") on every release from the one that shipped sp-qsona.
#
# CASES (law-absence-needs-a-positive-control):
#   1. END TO END, push mode: the real aeon.sh closes a task bead (shim) -> open+submitted;
#      the real landing.sh then lands the branch on origin/main and the bead ends CLOSED with
#      the submitted label gone from the ready set's point of view (closed).
#   2. A submitted bead the landing pass REOPENS (gate FAIL) loses the submitted label, so it
#      is claimable again — a reopen that kept it would strand it: open, excluded from every
#      claim, never landed.
#   3. pr/hold mode: the merge happens off-box, and the Sending is the first to see the work
#      on the base. A submitted bead whose branch is found landed there is closed.
#
# defect: sp-qsona (acceptance phase A stage 5)
# tier: T3
# covers: spira/landing.sh spira/sending.sh spira/pr-pass-branch.sh spira/lib.sh spira/aeon.sh
# hermetic-ok: uses a fixture database and local git repos, no systemd or gh
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-submitted-lands
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up submittedlands || { echo "test-submitted-lands: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

echo "test-submitted-lands.sh"

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null
git -C "$REPO" fetch -q origin
git -C "$REPO" remote set-head origin main 2>/dev/null || true

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
find "$HERE" -maxdepth 1 \( -name '*.sh' -o -name '*.py' \) ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN/worktree"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"

stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SPIRA_HOME/$1"; chmod +x "$SPIRA_HOME/$1"; }
stub confine.sh 'exit 0'
stub mail.sh    'exit 0'
stub gh         'exit 1'
gate_pass() { stub gate.sh 'echo "gate: VERDICT=PASS reason=stub branch=$1 repo=${2:-?}" >&2; exit 0'; }
gate_fail() { stub gate.sh 'echo "gate: VERDICT=FAIL reason=stub-fail branch=$1 repo=${2:-?}" >&2; exit 1'; }
gate_pass

cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

# THE SHIM IS THE SESSION: commit, then close the bead the way a builder does. conf.sh
# replaces PATH, so the model is injected through SPIRA_AGENT and nothing else.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-submitted-lands: aeon.sh has no SPIRA_AGENT injection point" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf '%s\n' "$id" > "$id.txt"
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id: the work"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

B() { bd -C "$SPIRA_DB" "$@"; }
field() { B show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]
v=d[0].get(sys.argv[1])
print(",".join(v) if isinstance(v,list) else (v or ""))' "$2" 2>/dev/null; }
seed() {   # seed <id> [status] [extra-label]
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\"${3:+,\"$3\"}"
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/aeon.out" 2>&1; }
landing() {
    rm -f "$SPIRA_RUN/landing.progress"
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=fixture SPIRA_ID_PREFIX=sp SPIRA_GH="$SPIRA_HOME/gh" \
        bash "$SPIRA_HOME/landing.sh" 2>&1
}
sending() {
    SPIRA_REPO="$REPO" SPIRA_HOME_REPO=fixture SPIRA_GH="$SPIRA_HOME/gh" \
        bash "$SPIRA_HOME/sending.sh" 2>&1
}
on_base() { git -C "$REPO" fetch -q origin 2>/dev/null; git -C "$REPO" log --format=%s origin/main 2>/dev/null; }

# ======================================================================================
echo
echo "1. END TO END, push mode — aeon close -> submitted -> landed -> closed:"
# ======================================================================================
testdb_reset; seed sp-sl-1
run_aeon
is   "after the aeon: the close was converted, the bead is open" open "$(field sp-sl-1 status)"
want "carrying the submitted label"                               "spira-submitted" "$(field sp-sl-1 labels)"
nowant "and nothing is on the base yet"                           "sp-sl-1" "$(on_base)"

out="$(landing)"
want "the landing pass lands the submitted bead's branch" "landed spira/sp-sl-1" "$out"
want "its commit is on origin/main"                        "sp-sl-1: the work" "$(on_base)"
is   "and the bead is closed by the landing"               closed "$(field sp-sl-1 status)"
want "with the landed outcome as its close reason"         "OUTCOME: landed" "$(field sp-sl-1 close_reason)"
nowant "never the 'not landed — its bead is open' skip"     "its bead is open" "$out"

# ======================================================================================
echo
echo "2. a submitted bead the landing pass REOPENS loses the label — claimable again:"
# ======================================================================================
testdb_reset; seed sp-sl-2
run_aeon
want "setup: submitted" "spira-submitted" "$(field sp-sl-2 labels)"
gate_fail
out="$(landing)"
gate_pass
want   "the failed gate reopens it"          "reopened sp-sl-2" "$out"
is     "the bead is open"                    open "$(field sp-sl-2 status)"
nowant "and no longer carries the label"     "spira-submitted" "$(field sp-sl-2 labels)"

# ======================================================================================
echo
echo "3. pr/hold mode — a submitted bead whose branch the Sending finds on the base closes:"
# ======================================================================================
# The forge (or a human) merged the branch; nothing on this box landed it. Simulated with a
# merge made directly on origin/main, then the Sending sweep.
testdb_reset; seed sp-sl-3 open spira-submitted
git -C "$REPO" branch -q -f spira/sp-sl-3 origin/main
git -C "$REPO" worktree add -q "$TMP/wt3" spira/sp-sl-3
printf 'three\n' > "$TMP/wt3/three.txt"
git -C "$TMP/wt3" add -A; git -C "$TMP/wt3" commit -qm "sp-sl-3: the work"
git -C "$REPO" worktree remove --force "$TMP/wt3"
git -C "$REPO" checkout -q main 2>/dev/null; git -C "$REPO" reset -q --hard origin/main
git -C "$REPO" merge -q --no-ff -m "Merge pull request #3 from spira/sp-sl-3" spira/sp-sl-3
git -C "$REPO" push -q origin main 2>/dev/null
git -C "$REPO" fetch -q origin
is   "setup: open and submitted before the sweep" open "$(field sp-sl-3 status)"
out="$(sending)"
want "the Sending sends the landed branch"             "sp-sl-3" "$out"
is   "and closes the submitted bead"                   closed "$(field sp-sl-3 status)"
want "with the landed outcome"                         "OUTCOME: landed" "$(field sp-sl-3 close_reason)"

tl_summary
