#!/usr/bin/env bash
#
# test-poison.sh — the sole T3 sentinel pass over CHECK 4: does the poison valve cover
#   every bead the summoner can dispatch, end to end, through the real sentinel.sh?
#
#   ./test-poison.sh
#
# THE ORIGINAL DEFECT THIS REPRODUCES. There were two predicates for "which beads are
# ours" and they disagreed. Summoning goes through fayth_ready, which asks each persona its
# own FAYTH_LABELS; the valve that stops a bead failing forever iterated the goal epic's
# children. A bead carrying a partition's labels but parented outside the goal was therefore
# dispatchable and unpoisonable — summoned every pass, failing every time, never reaching the
# valve that exists to stop exactly that.
#
# MERGED (sp-eq8a4.2.4, duplicate cluster D1/D3/D4, UC-aeon-execution-21/22/23): every case
# in test-poison-edge.sh (ask title/BRANCH wording, stale poison clear, thrash exemption, the
# POISON_AT=0 edge) and test-poison-ask.sh (ask-once-per-count dedup, closed-mid-pass, empty
# chamber) now lives here, in ONE SQL-seeded store, one bead per row, two sentinel passes —
# not three files each re-seeding the same ~100-line fixture. The per-decision arithmetic
# these files also asserted (threshold, dedup, cap math) moved to check4_decide and is
# covered at T1 by test-check4-unit.sh; what stays here is that the REAL sentinel.sh wires
# check4_decide's output to a real label, a real mail, a real event — the seam actually
# reaching the store, not a model of it.
#
# G11 (reclaim cap, sentinel.sh ~382-394): SPIRA_RECLAIM_AT was set by the now-deleted
# test-requeue-cap-accept.sh but nothing ever asserted it fired, and the sentinel's own loop
# hardcoded reclaims=0 — dead code standing in for the wiring. Closed below: a real
# 'reclaimed' event reaches check4_bulk_data's fourth column and a real mail goes out.
#
# G13 (cross-partition requeue cap, ex test-requeue-cap.sh mapper note): the `tinc` persona
# below is not decoration — a bead in ITS partition is cycled past the requeue cap too, so
# a green result cannot be a check that only happens to work for the one partition it was
# written against.
#
# EVERY CASE HERE IS A PAIR, because the whole defect is a set that LOOKS complete. Each
# poisoned bead is also asserted absent from goal_open_children — that is the proof the old
# code could not have found it — and each bead the valve must leave alone is paired with one
# it must take (law-absence-needs-a-positive-control).
#
# The database is a REAL bd on a fixture dropped by a trap, because what is under test is
# which beads a query returns and a model of bd would be a second implementation of the
# thing in question (law-prefer-the-real-dependency). The sub-programs ARE stubs: what they
# do is not under test here, only which beads the valve reaches.
#
# tier: T3
# defect: sp-mqnf sp-njwb sp-fx1p sp-pi3ez sp-wiyr2 sp-qd2ul
# covers: spira/sentinel.sh spira/lib.sh spira/attempts.sh spira/chamber/*
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require test-poison
TMP="$(mktemp -d)"; trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
# testdb-mode: server — sentinel's poison threshold reads attempts_of/check4_bulk_data via
# bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up poison || skip "server testdb not available"
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
cp "$HERE/sentinel.sh" "$HERE/lib.sh" "$HERE/landing.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$HERE/attempts.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand.sh     'printf "%s" "${STRAND_OUT:-}"'
stub sending.sh    'printf "%s" "${SENDING_OUT:-}"'
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
# which is what this one was, by another route. It also carries G13: `tinc`'s partition is
# exercised by name below, not left as an unused fixture.
#
# EACH DECLARES ITS OWN EXCLUSIONS, unexpanded, exactly as a shipped fayth does: the string
# is evaluated when the fayth is sourced, so the suite pins the escalation and CI labels to
# whatever the harness configures rather than to a literal written here twice.
printf 'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n'     > "$SH/chamber/t.fayth"
printf 'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_INCIDENT_LABEL}"\nFAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL,$SPIRA_CI_LABEL"\nFAYTH_MAX_CONCURRENT=0\n' > "$SH/chamber/tinc.fayth"

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
# attempts.sh under the same configuration as sentinel(), so a deadlock lift is made through
# the real tool against the real fixture repo — not a model of what it would do.
deadlocked() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" \
        bash "$SH/attempts.sh" deadlocked "$@" 2>&1
}
# attempts.sh clear, same configuration — the operator's own remedy against the real store.
clearpoison() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" \
        bash "$SH/attempts.sh" clear "$@" 2>&1
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
    # THE ASK'S SUPPRESSION AND THE LIFT'S DEDUP ARE BOTH MARKS IN THE RUN DIRECTORY, and
    # testdb_reset does not reach either — it resets the database, not $SPIRA_RUN. Without
    # clearing poison-lifted too, a later case that reuses a bead id at the SAME attempt
    # count a prior case's deadlocked/clear lifted at inherits that lift and is never
    # poisoned at all (seen when the sp-qd2ul case right after sp-wiyr2's reused sp-orphan
    # at count 3, the exact count sp-wiyr2's lift recorded).
    rm -rf "$RUN/poison-asked" "$RUN/requeue-asked" "$RUN/reclaim-asked" "$RUN/poison-lifted"
    testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-block","title":"the blocker","status":"open","issue_type":"task","labels":[],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-open","title":"blocked","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-open","depends_on_id":"sp-goal","type":"parent-child"},{"issue_id":"sp-open","depends_on_id":"sp-block","type":"blocks"}]}
JSONL
}

# `sp-orphan` IS the bug: it carries the builder's labels, so bd ready offers it and an aeon
# is summoned for it, and it has no parent at all. `sp-kid` is the case the old code did
# cover, kept so that the fix is shown not to be a swap.
#
# sp-attempt-N labels are no longer written (sp-lzt); sentinel CHECK4 reads attempt counts
# from status_changed events. cycle() creates the events by transitioning each bead to
# in_progress and back N times, matching POISON_AT=3 for orphan/kid and POISON_AT-1 for young.
POISON_SEED=$(cat <<JSONL
{"id":"sp-orphan","title":"dispatchable, unparented","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-kid","title":"a child of the goal","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z","dependencies":[{"issue_id":"sp-kid","depends_on_id":"sp-goal","type":"parent-child"}]}
{"id":"sp-young","title":"below the threshold","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
)
cycle() {   # cycle <id> <n> — create n status_changed(in_progress) events via bd update
    local id="$1" n="$2" i=0
    while [ "$i" -lt "$n" ]; do
        B update "$id" --status in_progress >/dev/null 2>&1
        B update "$id" --status open >/dev/null 2>&1
        i=$((i+1))
    done
}
seedn() {   # seedn <id> <event_type> <new_value> <n> — n raw events via bd sql
    local id="$1" et="$2" nv="$3" n="${4:-1}" i=0 uuid
    while [ "$i" -lt "$n" ]; do
        uuid="$(python3 -c 'import uuid; print(uuid.uuid4())')"
        B sql "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('$uuid', '$id', '$et', 'harness', '$nv', NOW())" >/dev/null 2>&1
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
want        "and the operator is asked what to do about it"  "3 in_progress transition(s) without landing (3 attempts)" "$(cat "$MAIL_LOG")"
# THE QUESTION MAIL CARRIES THE BEAD TITLE AND A DEFAULT LINE. The operator must be able to
# act without opening a second pane: the title says what the work was for, and the default
# says what to do if the right answer is not obvious.
want "the question mail body has the bead title"   "TITLE"       "$(cat "$MAIL_LOG")"
want "and a Default line the operator can follow"  "## Default"  "$(cat "$MAIL_LOG")"
want "and carries a failure excerpt (session log)" "--- last session log" "$(cat "$MAIL_LOG")"
# THE POISONING IS RECORDED AS AN EVENT in events.log, not the operator mailbox.
want "and the poisoning is recorded as an event" "kind: bead.poisoned" "$(cat "$RUN/events.log" 2>/dev/null)"
want "against the bead that poisoned"            "target: sp-orphan"   "$(cat "$RUN/events.log" 2>/dev/null)"
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
testdb_seed <<JSONL
{"id":"sp-epic","title":"an epic at the threshold","status":"open","issue_type":"epic","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
out="$(sentinel)"
notpoisoned "an epic is never poisoned" sp-epic

# --------------------------------------------------------------------------------------
# A POISONED BEAD KEEPS ITS CLAIM, AND STOPS BEING SUMMONED FOR.
#
# The lease is a REAL one taken by `bd ready --claim`, because what is under test is that
# the valve does not cut it: unclaiming here would pull the lease out from under a session
# still writing, and the aeon releases on its own exit path anyway.
# --------------------------------------------------------------------------------------
seed_held() {
    seed
    testdb_seed <<JSONL
{"id":"sp-orphan","title":"dispatchable, unparented","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
    cycle sp-orphan 3
    BEADS_ACTOR=aeon-holder B ready --claim --limit 0 --label "${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan}" >/dev/null 2>&1
}

seed_held
is "the fixture starts with the bead held" "in_progress" "$(status_of sp-orphan)"
is "and by a named holder"                 "aeon-holder" "$(assignee_of sp-orphan)"
out="$(sentinel)"
ispoisoned "a held bead at the threshold is still poisoned" sp-orphan
is   "but it is not unclaimed under its holder" "in_progress" "$(status_of sp-orphan)"
is   "and the holder is untouched"              "aeon-holder" "$(assignee_of sp-orphan)"
flat() { tr -s ' \n\t' ' ' <<<"$1"; }
want "the note says the holder keeps its claim" "releases on its own exit path" \
     "$(flat "$(B show sp-orphan 2>/dev/null)")"

# ...and once the holder lets go, CHECK 7 declines to summon for it.
release() { B update sp-orphan --status open >/dev/null 2>&1; B update sp-orphan --assignee "" >/dev/null 2>&1; }
release; out="$(sentinel)"
want "CHECK 7 declines to summon for a poisoned bead" "t: nothing ready in its partition" "$out"
B label remove sp-orphan spira-poison >/dev/null 2>&1
release; out="$(SPIRA_POISON_AT=99 sentinel)"
nowant "and would have summoned for it unpoisoned" "t: nothing ready in its partition" "$out"
want   "the same bead unpoisoned is ready for its fayth" "t: 1 ready" "$out"

# --------------------------------------------------------------------------------------
# ACCEPTANCE (sp-njwb, ex test-poison-edge.sh): ask title leads with charge reason; BRANCH
# line shows commit count.
# --------------------------------------------------------------------------------------
echo
seed_poison; rm -rf "$RUN/poison-asked"; : > "$MAIL_LOG"
git -C "$REPO" checkout -q -b "spira/sp-orphan" 2>/dev/null
git -C "$REPO" checkout -q main 2>/dev/null
out="$(sentinel)"
want "ask title shows attempt count not 'failed'" \
     "3 in_progress transition(s) without landing (3 attempts)" "$(cat "$MAIL_LOG")"
nowant "title does not contain 'failed N times'" \
       "failed 3 times" "$(cat "$MAIL_LOG")"
want "BRANCH line says no commits when branch is empty" \
     "no commits" "$(cat "$MAIL_LOG")"
nowant "BRANCH line does not claim work exists" \
       "with work on it" "$(cat "$MAIL_LOG")"

git -C "$REPO" checkout -q "spira/sp-orphan" 2>/dev/null
git -C "$REPO" commit -q --allow-empty -m "one unit of work" 2>/dev/null
git -C "$REPO" checkout -q main 2>/dev/null
seed_poison; rm -rf "$RUN/poison-asked"
: > "$MAIL_LOG"; out="$(sentinel)"
want "branch with one commit reports its count" "1 commit" "$(cat "$MAIL_LOG")"
nowant "and does not say no commits" "no commits" "$(cat "$MAIL_LOG")"
git -C "$REPO" branch -D "spira/sp-orphan" 2>/dev/null || true

# --------------------------------------------------------------------------------------
# STALE POISON CLEAR (sp-fx1p, ex test-poison-edge.sh): a bead whose attempt count drops
# below the threshold must have its spira-poison label removed automatically.
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<JSONL
{"id":"sp-stale","title":"stale poison — count below threshold","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","spira-poison"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-live","title":"live poison — count at threshold","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan","spira-poison"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
cycle sp-stale 1
cycle sp-live 3
out="$(SPIRA_POISON_AT=3 sentinel)"
notpoisoned "a poisoned bead with count below threshold has its label cleared"  sp-stale
ispoisoned  "a poisoned bead with count at threshold keeps its label"           sp-live
want        "the pass records the stale clear" "stale poison cleared" "$out"

# --------------------------------------------------------------------------------------
# THRASH REQUEUES DO NOT POISON (sp-pi3ez, ex test-poison-edge.sh).
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<JSONL
{"id":"sp-thrash","title":"thrash-only","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-real","title":"real failures","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
for i in 1 2 3; do
    B update sp-thrash --status in_progress >/dev/null 2>&1
    seedn sp-thrash requeued thrash 1
    B update sp-thrash --status open >/dev/null 2>&1
done
cycle sp-real 3
out="$(sentinel)"
notpoisoned "three thrash requeues do not poison the bead"  sp-thrash
ispoisoned  "CONTROL: three real failures still poison"     sp-real

# --------------------------------------------------------------------------------------
# ZERO CHARGED ATTEMPTS SENDS NO MAIL (POISON_AT=0 EDGE CASE, ex test-poison-edge.sh).
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked"; : > "$MAIL_LOG"
testdb_seed <<JSONL
{"id":"sp-zero","title":"zero attempts","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-one","title":"one attempt","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
cycle sp-one 1
out="$(SPIRA_POISON_AT=0 sentinel)"
notpoisoned "a bead with zero attempts is not poisoned even at POISON_AT=0" sp-zero
nowant      "and no mail is sent for it"                                    "sp-zero" "$(cat "$MAIL_LOG")"
ispoisoned  "CONTROL: a bead with one attempt is poisoned at POISON_AT=0"  sp-one
want        "and the operator is asked"                                     "sp-one"  "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# THE ASK IS FILED ONCE PER (BEAD, ATTEMPT COUNT), EVER (ex test-poison-ask.sh) — and its
# suppression is not the poison label.
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"; out="$(sentinel)"
is "the first pass over the threshold asks exactly once" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"
out="$(sentinel)"
is "a second pass over the same count asks nothing more" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

B label remove sp-orphan spira-poison >/dev/null 2>&1
out="$(sentinel)"
ispoisoned "the bead is poisoned again, because it is still over the threshold" sp-orphan
is "but clearing the label did NOT re-arm the ask" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

B label remove sp-orphan spira-poison >/dev/null 2>&1
cycle sp-orphan 1
out="$(sentinel)"
is "a fourth attempt is a new fact and asks again" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*4 attempts' "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# A CLOSED BEAD NEVER POISONS AND NEVER ASKS (ex test-poison-ask.sh).
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"
testdb_seed <<JSONL
{"id":"sp-late","title":"closed while the pass ran","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
cycle sp-late 3
out="$(ASK_CLOSES=sp-late sentinel)"
is          "the fixture really did close it mid-pass" "closed" "$(status_of sp-late)"
ispoisoned  "the bead that was still open is poisoned" sp-orphan
notpoisoned "the one that closed mid-pass is not"      sp-late
nowant "and the operator is not asked to drop landed work" "Spira bead sp-late" "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# AN EMPTY CHAMBER POISONS NOTHING AND SAYS SO (ex test-poison-ask.sh).
# --------------------------------------------------------------------------------------
echo
seed_poison; out="$(ROSTER=nosuchfayth sentinel)"
notpoisoned "an empty chamber poisons nothing" sp-orphan
want "and says no bead is being examined" "no bead is dispatchable" "$out"

# --------------------------------------------------------------------------------------
# G11 — THE RECLAIM CAP, WIRED END TO END. Before this seam, sentinel.sh's own per-bead
# loop hardcoded reclaims=0, so RECLAIM_AT could never fire no matter how many 'reclaimed'
# events a bead carried. This asserts the real event, the real bulk query and the real mail.
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked" "$RUN/reclaim-asked"; : > "$MAIL_LOG"
testdb_seed <<JSONL
{"id":"sp-reclaimed","title":"the box keeps killing its worker","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
seedn sp-reclaimed reclaimed '' 5
out="$(SPIRA_RECLAIM_AT=5 sentinel)"
want "the reclaim cap fires a real mail from real reclaimed events" \
     "5 aeons died holding it, work never judged" "$(cat "$MAIL_LOG")"
want "the mail says the box, not the work, is at fault" \
     "the box cannot run it" "$(cat "$MAIL_LOG")"
notpoisoned "a reclaim cap alone does not poison" sp-reclaimed
is "the reclaim ask is marked so a second pass does not repeat it" "yes" \
   "$([ -s "$RUN/reclaim-asked/sp-reclaimed" ] && echo yes || echo no)"
: > "$MAIL_LOG"; out="$(SPIRA_RECLAIM_AT=5 sentinel)"
nowant "a second pass over the same count sends nothing more" "sp-reclaimed" "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# G13 — THE REQUEUE CAP IS NOT HARD-CODED TO ONE PARTITION. `tinc` is the second fayth in
# this fixture's chamber; a bead carrying ITS labels, cycled past REQUEUE_AT, must be
# escalated exactly like a `t`-partition bead is.
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked" "$RUN/requeue-asked"; : > "$MAIL_LOG"
testdb_seed <<JSONL
{"id":"sp-tinc-req","title":"an incident-partition bead over the requeue cap","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","incident"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
seedn sp-tinc-req reopened '' 5
out="$(SPIRA_REQUEUE_AT=5 sentinel)"
want "the requeue cap fires for the tinc partition's own bead too" \
     "sp-tinc-req — completed and requeued 5 times" "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# sp-wiyr2 — A LIFT BY attempts.sh deadlocked SURVIVES THE NEXT SENTINEL PASS. deadlocked
# takes the spira-poison label off finished, mergeable work; it does not touch the attempt
# count (the rungs are the record of how the bead got here). Without the fix CHECK 4 reads
# that same unchanged count against a bead with no label on its very next pass and poisons
# it right back — the finished-work exemption undone within minutes of being granted.
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"; out="$(sentinel)"
ispoisoned "sp-orphan is poisoned by the first pass, same as always" sp-orphan
# THE DEADLOCK ITSELF: a branch naming the bead that merges cleanly into what it lands on.
git -C "$REPO" checkout -q -B spira/sp-orphan main
printf 'finished work\n' > "$REPO/g"; git -C "$REPO" add g
git -C "$REPO" commit -qm "sp-orphan — the work"
git -C "$REPO" checkout -q main
dl_out="$(deadlocked --apply)"
want "the sweep finds it finished and lifts the poison" "RESTORED sp-orphan" "$dl_out"
notpoisoned "the label is off right after the lift" sp-orphan
: > "$MAIL_LOG"; out="$(sentinel)"
notpoisoned "and it is STILL off after the very next sentinel pass" sp-orphan
nowant "no fresh poison ask went out for it either" "sp-orphan" "$(cat "$MAIL_LOG")"
want "the check log shows CHECK 4 actually examined the set" "CHECK4 examining" "$out"

# --------------------------------------------------------------------------------------
# sp-qd2ul — CLEARING THE POISON LABEL MUST STICK. The operator's own remedy (sentinel's ask
# tells a human to "clear spira-poison") is `attempts.sh clear`, not the tool-specific
# `deadlocked` sweep above — the bead need not be finished, only judged worth retrying. What
# is asserted here is the property the bare-removal case at line ~399 shows this codebase does
# NOT get for free: a clear survives a sentinel pass with nothing new against it, and is NOT a
# permanent exemption — a genuinely new run of failures after the clear poisons it again.
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"; out="$(sentinel)"
ispoisoned "sp-orphan is poisoned by the first pass" sp-orphan
cl_out="$(clearpoison sp-orphan --apply)"
want "attempts.sh clear reports the lift" "CLEARED  sp-orphan" "$cl_out"
notpoisoned "the label is off right after the clear" sp-orphan
: > "$MAIL_LOG"; out="$(sentinel)"
notpoisoned "and it is STILL off after the very next sentinel pass" sp-orphan
nowant "no fresh poison ask went out for it either" "sp-orphan" "$(cat "$MAIL_LOG")"
# THE NON-PERMANENCE CONTROL: three NEW in_progress transitions after the clear are a
# genuinely new fact, and the valve must still reach it (law-absence-needs-a-positive-control
# — a clear that could never poison again would look identical to one that works correctly).
cycle sp-orphan 3
out="$(sentinel)"
ispoisoned "a fresh run of failures after the clear poisons it again" sp-orphan
want "and the ask cites the count since the clear, not the total ever charged" \
     "3 in_progress transition(s) without landing (3 attempts)" "$(cat "$MAIL_LOG")"

tl_summary
