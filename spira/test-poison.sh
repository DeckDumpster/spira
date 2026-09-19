#!/usr/bin/env bash
#
# test-poison.sh — does the poison valve cover every bead the summoner can dispatch?
#
#   ./test-poison.sh
#
# THE DEFECT THIS REPRODUCES. There were two predicates for "which beads are ours" and they
# disagreed. Summoning goes through fayth_ready, which asks each persona its own
# FAYTH_LABELS; the valve that stops a bead failing forever iterated the goal epic's
# children. A bead carrying a partition's labels but parented outside the goal was therefore
# dispatchable and unpoisonable — summoned every pass, failing every time, never reaching the
# valve that exists to stop exactly that. Measured on one live database: 8 children examined
# standing for 66 beads dispatched, with one bead at 9 attempts against a threshold of 3.
#
# EVERY CASE HERE IS A PAIR, because the whole defect is a set that LOOKS complete. Each
# poisoned bead is also asserted absent from goal_open_children — that is the proof the old
# code could not have found it — and each bead the valve must leave alone is paired with one
# it must take (law-absence-needs-a-positive-control).
#
# AND THE STATUS FILTER IS NOT THE BUG, which matters because a fix aimed at it would change
# nothing and read as a fix. goal_open_children returns every non-closed child, in_progress
# included; the first assertion below is what rules that out before anything else.
#
# The database is a REAL bd on a fixture dropped by a trap, because what is under test is
# which beads a query returns and a model of bd would be a second implementation of the
# thing in question (law-prefer-the-real-dependency). The sub-programs ARE stubs: what they
# do is not under test here, only which beads the valve reaches.
#
# defect: sp-mqnf
# covers: spira/sentinel.sh spira/lib.sh spira/chamber/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-poison
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
export SPIRA_TESTDB_MODE=server
testdb_up poison || {
    printf 'SKIP test-poison: server testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

REPO="$TMP/repo"; RUN="$TMP/run"; REMOTE="$TMP/remote.git"; SH="$TMP/spira"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m base
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q origin main
git -C "$REPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH/chamber"

# The program under test, run out of its own directory so it sources the real lib.sh but
# finds stubbed sub-programs beside it.
cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand.sh     'printf "%s" "${STRAND_OUT:-}"'
stub sending.sh    'printf "%s" "${SENDING_OUT:-}"'
stub governor.sh   'exit 0'
stub gate.sh       'exit ${GATE_RC:-0}'
stub reflect.sh    'touch "$SPIRA_RUN/reflect.fired"'
# mail.sh is RECORDED, not merely swallowed: half of what poisoning must do is reach the
# operator, and a stub that exits 0 without a trace would pass whether or not it ran.
# ASK_CLOSES is the seam that stages a race no fixture can otherwise produce: a bead that is
# dispatchable when the pass snapshots the set and CLOSED by the time the loop reaches it. The
# stub closes the named bead the first time it is called about any OTHER bead, which is exactly
# a landing finishing mid-pass.
stub mail.sh       '[ "${1:-}" = send ] || exit 0
printf "%s\n" "$*" >> "$MAIL_LOG"
cat >> "$MAIL_LOG"
if [ -n "${ASK_CLOSES:-}" ]; then case "$*" in *"$ASK_CLOSES"*) ;;
    *) bd -C "$SPIRA_DB" close "$ASK_CLOSES" --reason landed >/dev/null 2>&1 ;; esac; fi'

# TWO PERSONAS, EACH WITH A PARTITION OF ITS OWN, because a single-persona chamber cannot
# tell a valve that sweeps THE CHAMBER apart from one that sweeps a hardcoded partition —
# which is what this one was, by another route.
#
# EACH DECLARES ITS OWN EXCLUSIONS, unexpanded, exactly as a shipped fayth does: the string
# is evaluated when the fayth is sourced, so the suite pins the escalation and CI labels to
# whatever the harness configures rather than to a literal written here twice.
printf 'FAYTH_LABELS="spira,plan"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n'     > "$SH/chamber/t.fayth"
printf 'FAYTH_LABELS="spira,incident"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/tinc.fayth"

B() { bd -C "$SPIRA_DB" "$@"; }
export MAIL_LOG="$TMP/mail.log"; : > "$MAIL_LOG"
cat > "$TMP/launch" <<'L'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$LAUNCH_LOG"
exit "${LAUNCH_RC:-0}"
L
cat > "$TMP/systemctl" <<'S'
#!/usr/bin/env bash
printf '%s\n' "${LAND_STATE:-inactive}"
S
chmod +x "$TMP/launch" "$TMP/systemctl"
export LAUNCH_LOG="$TMP/launch.log"

# Concurrency 0 in both fayths, so CHECK 7 never reaches systemd-run: a summon in a test
# would put a real aeon on a real database.
sentinel() {
    rm -f "$RUN/reflect.fired" "$RUN/inference.cooldown"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
    SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" SPIRA_INFERENCE_EVERY=0 \
    SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
    SPIRA_SUMMON="$TMP/launch" \
    SPIRA_SKIP_RECLAIM=1 \
    SPIRA_SKIP_CLOSED_CHECK=1 \
        bash "$SH/sentinel.sh" 2>&1
}

# lib.sh under the same configuration, so the two set predicates can be asked directly
# rather than inferred from a pass's output.
predicate() {   # predicate <fn> -> that lib predicate's output under the fixture
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_GOAL=sp-goal \
    SPIRA_FAYTHS="${ROSTER:-t tinc}" \
        bash -c ". \"$SH/lib.sh\"; $1" 2>/dev/null
}
labels_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(" ".join(d[0].get("labels") or []))'; }
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
assignee_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("assignee") or "")'; }
poisoned()    { [[ " $(labels_of "$1") " == *" spira-poison "* ]]; }
ispoisoned()  { poisoned "$2" && ok "$1" || bad "$1" "$2 was not poisoned"; }
notpoisoned() { poisoned "$2" && bad "$1" "$2 was poisoned" || ok "$1"; }

# Parenthood is a `parent-child` dependency, which is how the live database expresses it:
# `bd children` is an alias for `bd list --parent`, and a bare "parent" field on an import
# row creates no edge at all.
seed() {   # seed — the goal, one unclaimable child of it, and that child's blocker
    testdb_reset
    # The ask's suppression is a mark in the run directory and testdb_reset does not reach it,
    # so a case that did not clear it would inherit the previous case's silence.
    rm -rf "$RUN/poison-asked"
    testdb_seed <<'JSONL'
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-block","title":"the blocker","status":"open","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-open","title":"blocked","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-open","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-open","depends_on_id":"sp-block","type":"blocks"}]}
JSONL
}

# `sp-orphan` IS the bug: it carries the builder's labels, so bd ready offers it and an aeon
# is summoned for it, and it has no parent at all. `sp-kid` is the case the old code did
# cover, kept so that the fix is shown not to be a swap.
#
# sp-attempt-N labels are no longer written (sp-lzt); sentinel CHECK4 reads attempt counts
# from status_changed events. cycle() creates the events by transitioning each bead to
# in_progress and back N times, matching POISON_AT=3 for orphan/kid and POISON_AT-1 for young.
POISON_SEED='{"id":"sp-orphan","title":"dispatchable, unparented","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-kid","title":"a child of the goal","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-young","title":"below the threshold","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}'
cycle() {   # cycle <id> <n> — create n status_changed(in_progress) events via bd update
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B update "$id" --status in_progress >/dev/null 2>&1
        B update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}
seed_poison() {
    seed
    testdb_seed <<< "$POISON_SEED"
    cycle sp-orphan 3   # at POISON_AT=3
    cycle sp-kid 3      # at POISON_AT=3
    cycle sp-young 2    # below threshold
}

echo "test-poison.sh"

# --------------------------------------------------------------------------------------
# THE CAUSE, ESTABLISHED BEFORE THE FIX. An in_progress child IS returned by
# goal_open_children — so sp-orphan is not missing from that set because of a status race,
# and a change to the status filter would be a fix to nothing.
# --------------------------------------------------------------------------------------
seed_poison
B update sp-kid --status in_progress >/dev/null 2>&1
want "goal_open_children returns in_progress beads too" "sp-kid" "$(predicate goal_open_children)"
B update sp-kid --status open >/dev/null 2>&1
nowant "a dispatchable unparented bead is not among the goal's children" \
       "sp-orphan" "$(predicate goal_open_children)"
want   "but it IS in the set the summoner can dispatch" \
       "sp-orphan" "$(predicate dispatchable_open)"

# --------------------------------------------------------------------------------------
# THE VALVE COVERS THAT SET.
# --------------------------------------------------------------------------------------
out="$(sentinel)"
ispoisoned  "a dispatchable bead at the threshold is poisoned"  sp-orphan
want        "and the pass says so"          "poisoned sp-orphan after 3 attempts" "$out"
# sp-attempt-N labels removed (sp-lzt); sentinel no longer reports per-cause breakdown.
# deleted: "unrecorded x3 (3 attempts)" — old label-derived cause summary in ask title.
want        "and the operator is asked what to do about it"  "3 in_progress transition(s) without landing (3 attempts)" "$(cat "$MAIL_LOG")"
# THE QUESTION MAIL CARRIES THE BEAD TITLE AND A DEFAULT LINE. The operator must be able to
# act without opening a second pane: the title says what the work was for, and the default
# says what to do if the right answer is not obvious.
want "the question mail body has the bead title"   "TITLE"       "$(cat "$MAIL_LOG")"
want "and a Default line the operator can follow"  "## Default"  "$(cat "$MAIL_LOG")"
want "and carries a failure excerpt (session log)" "--- last session log" "$(cat "$MAIL_LOG")"
# THE POISONING IS RECORDED AS AN EVENT in events.log, not the operator mailbox. The ask is
# the operator notification; the event log records the transition for audit. The transition
# fires once — on entry to poisoned; the label is now on the bead, so every later pass takes
# the other branch.
want "and the poisoning is recorded as an event" "kind: bead.poisoned" "$(cat "$RUN/events.log" 2>/dev/null)"
want "against the bead that poisoned"            "target: sp-orphan"   "$(cat "$RUN/events.log" 2>/dev/null)"
# THE EVENT DOES NOT MAIL. The operator mailbox holds the question (the ask); the event log
# holds the record of the transition. Neither must bleed into the other.
nowant "the event mail is not in the operator mailbox" "kind: bead.poisoned" "$(cat "$MAIL_LOG")"
# AND ONLY ON THE TRANSITION. The next pass sees spira-poison on the bead and takes the `;;`
# branch — the spira_event call is never reached.
: > "$MAIL_LOG"; out="$(sentinel)"
nowant "an already-poisoned bead emits nothing new to mail" "3 in_progress" "$(cat "$MAIL_LOG")"
ispoisoned  "a goal child at the threshold is poisoned too"    sp-kid
notpoisoned "and a bead below the threshold is left alone"     sp-young
want        "the check names the size of the set it examined"  "CHECK4 examining" "$out"

# The exclusions are the partition's OWN, so the valve and the claim agree by construction:
# a bead waiting on the operator is not dispatchable and must not be poisoned for waiting.
seed_poison; B label add sp-orphan "$SPIRA_ASK_LABEL" >/dev/null 2>&1
out="$(sentinel)"
notpoisoned "a bead the partition excludes is never poisoned" sp-orphan
nowant "and it is not in the dispatchable set" "sp-orphan" "$(predicate dispatchable_open)"

# An epic is a container. The summoner passes --exclude-type epic and never claims one, so
# poisoning one would take a pilgrimage out of circulation for its children's failures.
seed_poison
testdb_seed <<'JSONL'
{"id":"sp-epic","title":"an epic at the threshold","status":"open","issue_type":"epic","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
out="$(sentinel)"
notpoisoned "an epic is never poisoned" sp-epic

# --------------------------------------------------------------------------------------
# A POISONED BEAD KEEPS ITS CLAIM, AND STOPS BEING SUMMONED FOR.
#
# The lease is a REAL one taken by `bd ready --claim`, because what is under test is that
# the valve does not cut it: unclaiming here would pull the lease out from under a session
# still writing, and the aeon releases on its own exit path anyway.
#
# ONE DISPATCHABLE BEAD IN THE PARTITION, so the claim is deterministic and — the half that
# matters — so the CHECK 7 assertion below is about THIS bead and not a second one that
# happened to be ready. The base fixture's own plan bead is blocked, so it is neither. The
# holder is named by the suite rather than inherited: bd takes the assignee from
# BEADS_ACTOR, so a suite run from inside a live aeon would assert against that session.
# --------------------------------------------------------------------------------------
seed_held() {
    seed
    testdb_seed <<'JSONL'
{"id":"sp-orphan","title":"dispatchable, unparented","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
    # sp-attempt-N labels retired (sp-lzt); cycle creates status_changed events.
    # cycle 3 creates 3 events; the subsequent B ready --claim creates a 4th (claimed event).
    # attempts_of() counts both; 4 >= POISON_AT=3 triggers poisoning.
    cycle sp-orphan 3
    BEADS_ACTOR=aeon-holder B ready --claim --limit 0 --label spira,plan >/dev/null 2>&1
}

seed_held
is "the fixture starts with the bead held" "in_progress" "$(status_of sp-orphan)"
is "and by a named holder"                 "aeon-holder" "$(assignee_of sp-orphan)"
out="$(sentinel)"
ispoisoned "a held bead at the threshold is still poisoned" sp-orphan
is   "but it is not unclaimed under its holder" "in_progress" "$(status_of sp-orphan)"
is   "and the holder is untouched"              "aeon-holder" "$(assignee_of sp-orphan)"
# WHITESPACE-NORMALISED, because `bd show` WRAPS a note to the terminal width: the phrase
# being looked for is in the bead, but a newline lands in the middle of it as soon as
# anything earlier in the note changes its length. Matching the rendering rather than the
# content makes an unrelated edit fail a test that is not about it.
flat() { tr -s ' \n\t' ' ' <<<"$1"; }
want "the note says the holder keeps its claim" "releases on its own exit path" \
     "$(flat "$(B show sp-orphan 2>/dev/null)")"
# sp-lzt removed the per-cause breakdown from the poison note; the note now says
# "Poisoned after N in_progress transition(s)" rather than "charged by: N#cause".
# deleted: "and names the outcomes that charged it" via "charged by: 3#unrecorded"

# ...and once the holder lets go, CHECK 7 declines to summon for it. The pair is the point:
# the same fixture with the label cleared IS summoned for, so a green result here cannot be
# a fayth that had nothing ready for some other reason. The control raises the threshold
# rather than lowering the bead's attempts, so CHECK 4 does not simply re-poison it before
# CHECK 7 is reached and exactly one thing differs at CHECK 7: the label.
release() { B update sp-orphan --status open >/dev/null 2>&1; B update sp-orphan --assignee "" >/dev/null 2>&1; }
release; out="$(sentinel)"
want "CHECK 7 declines to summon for a poisoned bead" "t: nothing ready in its partition" "$out"
B label remove sp-orphan spira-poison >/dev/null 2>&1
release; out="$(SPIRA_POISON_AT=99 sentinel)"
nowant "and would have summoned for it unpoisoned" "t: nothing ready in its partition" "$out"
want   "the same bead unpoisoned is ready for its fayth" "t: 1 ready" "$out"

# --------------------------------------------------------------------------------------
# THE ASK IS FILED ONCE PER (BEAD, ATTEMPT COUNT), EVER — AND ITS SUPPRESSION IS NOT THE
# POISON LABEL.
#
# THE DEFECT. The valve asked whenever a bead was over the threshold and did not currently
# carry `spira-poison`, so the label was both the dispatch valve and the ask's only
# suppression — and the ask's own recommended remedy is "change the approach, then clear the
# label". Doing what it asks therefore deleted the only thing stopping it being asked again.
# One bead reached the operator three times in forty minutes about work that had already
# landed, and he had to say so twice.
#
# The pair is the whole point: clearing the label must ALLOW THE RETRY — which is what the
# remedy is for, and the only way back onto the board, since every partition excludes
# spira-poison — and must NOT re-arm the ask.
# --------------------------------------------------------------------------------------
seed_poison; : > "$MAIL_LOG"; out="$(sentinel)"
is "the first pass over the threshold asks exactly once" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"
out="$(sentinel)"
is "a second pass over the same count asks nothing more" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

# THE REMEDY IS APPLIED, exactly as the ask instructs. Nothing else changes: the count still
# stands, which is the state the old code re-asked from on every pass.
B label remove sp-orphan spira-poison >/dev/null 2>&1
out="$(sentinel)"
ispoisoned "the bead is poisoned again, because it is still over the threshold" sp-orphan
is "but clearing the label did NOT re-arm the ask" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

# ...and a genuinely NEW failure does ask again, which is the positive control on all of the
# above: a suppression that never lifts is indistinguishable from an ask that never fires.
B label remove sp-orphan spira-poison >/dev/null 2>&1
# sp-attempt-4-unlanded label retired (sp-lzt); one more in_progress cycle creates the 4th event.
cycle sp-orphan 1
out="$(sentinel)"
is "a fourth attempt is a new fact and asks again" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*4 attempts' "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# A CLOSED BEAD NEVER POISONS AND NEVER ASKS. dispatchable_open excludes closed beads, but it
# is a SNAPSHOT and this loop makes several bd calls per bead — so a bead the landing pass
# finished mid-pass was still in the list, and the operator was asked whether to change the
# approach on work that had already landed.
#
# The late bead sits in the OTHER partition, which is what makes the ordering deterministic:
# dispatchable_open iterates the roster in order, so the builder's bead is asked about first
# and the incident bead is still ahead of the loop when the stub closes it.
# --------------------------------------------------------------------------------------
seed_poison; : > "$MAIL_LOG"
testdb_seed <<'JSONL'
{"id":"sp-late","title":"closed while the pass ran","status":"open","issue_type":"task","labels":["spira","incident"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
# sp-attempt-3-unlanded label retired (sp-lzt); cycle creates 3 status_changed events.
cycle sp-late 3
out="$(ASK_CLOSES=sp-late sentinel)"
is          "the fixture really did close it mid-pass" "closed" "$(status_of sp-late)"
ispoisoned  "the bead that was still open is poisoned" sp-orphan
notpoisoned "the one that closed mid-pass is not"      sp-late
nowant "and the operator is not asked to drop landed work" "Spira bead sp-late" "$(cat "$MAIL_LOG")"
# sp-lzt: attempts_of() subtracts the close event; sp-late's count drops to 2 (<POISON_AT=3)
# when the bead closes mid-pass, so sentinel silently skips it rather than printing a
# "closed while this pass ran" message. The core property still holds: not poisoned, not asked.
# deleted: "the pass says why it declined" via "sp-late: 3 attempts, but it closed while this pass ran"

# A chamber that declares no partition dispatches nothing and examines nothing, and SAYS so.
# Nothing over the threshold and nothing looked at are the same silence otherwise.
seed_poison; out="$(ROSTER=nosuchfayth sentinel)"
notpoisoned "an empty chamber poisons nothing" sp-orphan
want "and says no bead is being examined" "no bead is dispatchable" "$out"

# --------------------------------------------------------------------------------------
# ACCEPTANCE (sp-njwb): ask title leads with charge reason; BRANCH line shows commit count.
#
# Two defects: (1) the title said "failed N times" whether the charge was closed-not-landed
# or a genuine fault — sending the operator looking for an error that may not exist; (2) the
# BRANCH line used show-ref, which returns true for a branch with no commits ahead of base,
# and rendered "with work on it" when none existed.
#
# The repo fixture has origin/main set up from the test preamble. spira_landref resolves it
# for the commit-count check.
# --------------------------------------------------------------------------------------
seed_poison; rm -rf "$RUN/poison-asked"; : > "$MAIL_LOG"
# Create the branch with no commits ahead of main.
git -C "$REPO" checkout -q -b "spira/sp-orphan" 2>/dev/null
git -C "$REPO" checkout -q main 2>/dev/null
out="$(sentinel)"
# sp-attempt-N labels retired (sp-lzt); sentinel no longer derives per-cause summary.
# deleted: "unrecorded x3 (3 attempts)" — old label-derived charge_summary format.
want "ask title shows attempt count not 'failed'" \
     "3 in_progress transition(s) without landing (3 attempts)" "$(cat "$MAIL_LOG")"
nowant "title does not contain 'failed N times'" \
       "failed 3 times" "$(cat "$MAIL_LOG")"
want "BRANCH line says no commits when branch is empty" \
     "no commits" "$(cat "$MAIL_LOG")"
nowant "BRANCH line does not claim work exists" \
       "with work on it" "$(cat "$MAIL_LOG")"

# Positive control: a branch with one commit shows the count, not "no commits".
# seed_poison resets the database so sp-orphan loses spira-poison and is again dispatchable.
git -C "$REPO" checkout -q "spira/sp-orphan" 2>/dev/null
git -C "$REPO" commit -q --allow-empty -m "one unit of work" 2>/dev/null
git -C "$REPO" checkout -q main 2>/dev/null
seed_poison; rm -rf "$RUN/poison-asked"
: > "$MAIL_LOG"; out="$(sentinel)"
want "branch with one commit reports its count" "1 commit" "$(cat "$MAIL_LOG")"
nowant "and does not say no commits" "no commits" "$(cat "$MAIL_LOG")"
git -C "$REPO" branch -D "spira/sp-orphan" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# STALE POISON CLEAR (sp-fx1p): a bead whose attempt count drops below the threshold
# must have its spira-poison label removed automatically. Without this the label becomes
# permanent: dispatchable_open excludes poisoned beads, so CHECK 4 never evaluates them
# again, and nothing can clear the label — a circular dependency (defect sp-9szt).
#
# TWO BEADS, ONE CLEARED AND ONE NOT. If both were cleared, a sentinel that blindly
# removes all poison would pass. If neither were cleared, a sentinel that clears nothing
# would pass. The pair proves the predicate: count < threshold -> clear; count >= threshold
# -> hold (law-absence-needs-a-positive-control).
#
# The beads START with spira-poison already on them — as if an operator removed some
# attempt labels from a previously-poisoned bead but did not remove the poison label.
# --------------------------------------------------------------------------------------
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<'JSONL'
{"id":"sp-stale","title":"stale poison — count below threshold","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-live","title":"live poison — count at threshold","status":"open","issue_type":"task","labels":["spira","plan","spira-poison"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
# sp-attempt-N labels retired (sp-lzt); cycle creates status_changed events for each bead.
# sp-stale needs 1 event (below threshold 3); sp-live needs 3 events (at threshold).
cycle sp-stale 1
cycle sp-live 3
out="$(SPIRA_POISON_AT=3 sentinel)"
notpoisoned "a poisoned bead with count below threshold has its label cleared"  sp-stale
ispoisoned  "a poisoned bead with count at threshold keeps its label"           sp-live
want        "the pass records the stale clear" "stale poison cleared" "$out"

# --------------------------------------------------------------------------------------
# THRASH REQUEUES DO NOT POISON (sp-pi3ez). A thrash requeue precedes a claim that writes
# an in_progress event; before this fix attempts_of counted that claim, so three thrash
# requeues reached POISON_AT and sent "change the approach" to the operator about work
# that had never been attempted. The fix: attempts_of subtracts requeued/thrash events.
#
# PAIR: three thrash requeues must not poison; three real failures must still poison.
# --------------------------------------------------------------------------------------
thrash_event() {   # thrash_event <id> — write one requeued/thrash event the way bump_requeue does
    local id="$1" uuid
    uuid="$(python3 -c 'import uuid; print(str(uuid.uuid4()))')"
    B sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', 'requeued', 'harness', 'thrash', NOW())" >/dev/null 2>&1
}
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<'JSONL'
{"id":"sp-thrash","title":"thrash-only","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-real","title":"real failures","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
# Three claim+thrash pairs: each B update writes a status_changed(in_progress) event;
# thrash_event writes the requeued/thrash that attempts_of must subtract.
for i in 1 2 3; do
    B update sp-thrash --status in_progress >/dev/null 2>&1
    thrash_event sp-thrash
    B update sp-thrash --status open >/dev/null 2>&1
done
cycle sp-real 3
out="$(sentinel)"
notpoisoned "three thrash requeues do not poison the bead"  sp-thrash
ispoisoned  "CONTROL: three real failures still poison"     sp-real

# --------------------------------------------------------------------------------------
# ZERO CHARGED ATTEMPTS SENDS NO MAIL (POISON_AT=0 EDGE CASE). When POISON_AT is zero
# every bead is at or above the threshold before a single attempt is charged. The guard
# must prevent both poisoning and asking in this degenerate configuration — nothing failed
# and there is nothing to report.
#
# PAIR: a bead with n=0 and POISON_AT=0 must send no mail; a bead with n=1 and POISON_AT=0
# must poison and ask. The control proves the guard is keyed on n, not on POISON_AT.
# --------------------------------------------------------------------------------------
seed; rm -rf "$RUN/poison-asked"; : > "$MAIL_LOG"
testdb_seed <<'JSONL'
{"id":"sp-zero","title":"zero attempts","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-one","title":"one attempt","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
# sp-one gets one in_progress cycle; sp-zero gets none.
cycle sp-one 1
out="$(SPIRA_POISON_AT=0 sentinel)"
notpoisoned "a bead with zero attempts is not poisoned even at POISON_AT=0" sp-zero
nowant      "and no mail is sent for it"                                    "sp-zero" "$(cat "$MAIL_LOG")"
ispoisoned  "CONTROL: a bead with one attempt is poisoned at POISON_AT=0"  sp-one
want        "and the operator is asked"                                     "sp-one"  "$(cat "$MAIL_LOG")"

printf '\ntest-poison.sh: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
