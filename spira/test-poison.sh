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
# THE POISON HOLD IS SPIRA-LC'S, NOT A bd LABEL (sp-i2m7y): CHECK 4 reads and writes it
# through spira-lc, so this suite starts its own throwaway `dolt sql-server` for
# spira_lifecycle (never dolt-beads.service) and builds a real spira-lc binary, the same
# shape test-lifecycle-container.sh and test-lifecycle-cutover.sh already use — a stub
# lc_hold/lc_held would only prove this suite's own model of spira-lc agrees with itself.
#
# tier: T3
# defect: sp-mqnf sp-njwb sp-fx1p sp-pi3ez sp-wiyr2 sp-qd2ul
# covers: sentinel/src/* spira/lib.sh spira/groomer.sh spira-claim/* spira/chamber/* lifecycle/* spira-lc/*
# timeout: 240
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

DOLT_BIN="$(command -v dolt 2>/dev/null || true)"
[ -n "$DOLT_BIN" ] || skip "dolt not found on PATH — install dolt before running this suite"

. "$HERE/testdb.sh"
testdb_available || skip "no fixture database reachable"
testdb_require test-poison
TMP="$(mktemp -d)"; trap 'testdb_drop; [ -n "${LC_SERVER_PID:-}" ] && kill "$LC_SERVER_PID" >/dev/null 2>&1; rm -rf "$TMP"' EXIT INT TERM
export PATH="$(dirname "$DOLT_BIN"):$PATH"
unset SPIRA_LC_SOCKET
# testdb-mode: server — sentinel's poison threshold reads attempts_of/check4_bulk_data via
# bd sql, which embedded mode refuses.
export SPIRA_TESTDB_MODE=server
testdb_up poison || skip "server testdb not available"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

SRC_ROOT="$(cd "$HERE/.." && pwd)"
LC_PORT=$((SPIRA_LC_TESTDB_PORT + 1000 + (RANDOM % 500)))
LC_TMP="$TMP/lc"; mkdir -p "$LC_TMP/data"
cat > "$LC_TMP/server.yaml" <<YAML
log_level: warning
listener:
  port: $LC_PORT
  max_connections: 100
  read_timeout_millis: 30000
  write_timeout_millis: 30000
data_dir: "$LC_TMP/data"
behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
YAML
"$DOLT_BIN" sql-server --config "$LC_TMP/server.yaml" > "$LC_TMP/server.log" 2>&1 &
LC_SERVER_PID=$!
lc_up=0
for _ in $(seq 1 50); do
    if "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls sql -q "SELECT 1" >/dev/null 2>&1; then
        lc_up=1; break
    fi
    sleep 0.2
done
[ "$lc_up" = 1 ] || bail "dolt sql-server for spira_lifecycle never came up: $(cat "$LC_TMP/server.log")"
lc_root_sql() { "$DOLT_BIN" --data-dir "$LC_TMP" --host 127.0.0.1 --port "$LC_PORT" -u root -p "" --no-tls "$@"; }

# The tree's spira-lc, by name on the suite's PATH (sp-gypjk).
LC_BIN=spira-lc
command -v "$LC_BIN" >/dev/null 2>&1 || bail "spira-lc is not on PATH"

export SPIRA_LC_HOST=127.0.0.1
export SPIRA_LC_PORT="$LC_PORT"
export SPIRA_LC_DB=spira_lifecycle
export SPIRA_LC_DATA_DIR="$LC_TMP"
export SPIRA_LC_USER=root
export SPIRA_LC_PASSWORD=""
"$LC_BIN" admin-apply-ddl "$SRC_ROOT/lifecycle/schema.sql" >"$LC_TMP/schema.log" 2>&1
wantrc "spira_lifecycle schema applies cleanly" 0 $?
LC_PASS="test-pass-$$"
sed "s/@SPIRA_LC_PASSWORD@/$LC_PASS/" "$SRC_ROOT/lifecycle/grants.sql" > "$LC_TMP/grants_filled.sql"
lc_root_sql sql < "$LC_TMP/grants_filled.sql" >"$LC_TMP/grants.log" 2>&1
wantrc "spira_lifecycle grants apply cleanly" 0 $?
# From here on spira-lc runs as spira_lc, the real production grant set — never root.
export SPIRA_LC_USER=spira_lc
export SPIRA_LC_PASSWORD="$LC_PASS"
# The poison valve's hold lives in spira-lc (sp-i2m7y): the machine is this suite's subject,
# so the sentinel runs with the switch ON. OFF (the default) writes the legacy spira-poison
# bd label instead — sentinel/src/check4.rs unit tests.
export SPIRA_LIFECYCLE_ENFORCE=1

# mklc <id>... — a fresh READY row for each id, dropping any row a prior scenario left
# behind (bd's own fixture resets on every seed/seed_poison call via testdb_reset, but the
# lifecycle server is a single long-lived instance across this whole suite, so a hold or
# state a previous scenario applied would otherwise leak into the next one's assertions).
mklc() {
    local i
    for i in "$@"; do
        lc_root_sql --use-db spira_lifecycle sql -q "DELETE FROM bead WHERE bead_id = '$i'" >/dev/null 2>&1
        "$LC_BIN" create-bead "$i" >/dev/null 2>&1
    done
}
# mkpoison <id>... — mklc, then a real Hold{Poison} event through `spira-lc hold`, for
# fixtures that need to start already poisoned (the stale-clear scenario below).
mkpoison() {
    local i
    for i in "$@"; do
        mklc "$i"
        "$LC_BIN" hold "$i" poison "seed" test >/dev/null 2>&1
    done
}
# rmpoison <id>... — the test-side equivalent of a human clearing the hold directly
# (bypassing spira-claim's `deadlocked`/`unpoison`, which have their own coverage elsewhere),
# via the same `spira-lc unhold` this suite asserts the sentinel's own CHECK 4 reads.
rmpoison() {
    local i
    for i in "$@"; do
        "$LC_BIN" unhold "$i" poison test >/dev/null 2>&1
    done
}
# lcheld <id> -> 0 if spira-lc currently holds the poison kind on <id> — the real
# `spira-lc held` (the caller verb that replaced lc.sh's lc_held, sp-arpjt), not a model of it.
lcheld() {
    "$LC_BIN" held "$1" poison
}

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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/suite-covers.sh" "$HERE/groomer.sh" "$SH/"
stub() { printf '#!/usr/bin/env bash\n%s\n' "$2" > "$SH/$1"; chmod +x "$SH/$1"; }
stub pilgrimage.sh 'printf "%s" "${PILGRIMAGE_OUT:-}"'
stub strand        'printf "%s" "${STRAND_OUT:-}"'
# THE RUST SENTINEL (sentinel.sh is gone) and spira-claim are the tree's, by name on PATH;
# the stubbed sub-programs in $SH are injected by putting $SH first on the sentinel's PATH.
stub sending       'printf "%s" "${SENDING_OUT:-}"'
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
#
# CHECK 4 (poison) NOW RUNS ONLY UNDER `--audit` (sp-994y9), decoupled from CHECK 7
# (summon) so a slow poison/closed/sending walk cannot starve the fleet of a fast summon
# cadence. This suite tests both from one call: run the audit half first (the poisoning
# this whole file is about) so its labels are on the store before the normal half reads
# them, then the normal half (summon, land-dispatch) — same order as production, where the
# normal pass dispatches audit and moves on rather than waiting for it. Concatenated so
# every existing assertion against "$out" still finds whichever half's line it wants.
sentinel() {
    rm -f "$RUN/reflect.fired" "$RUN/inference.cooldown"
    local audit_out normal_out
    audit_out="$(
        SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO="$REPO" \
        SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" SPIRA_INFERENCE_EVERY=0 \
        SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
        SPIRA_SUMMON="$TMP/launch" \
        SPIRA_SKIP_RECLAIM=1 \
        SPIRA_SKIP_CLOSED_CHECK=1 PATH="$SH:$PATH" \
            command sentinel --audit 2>&1
    )"
    normal_out="$(
        SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" \
        SPIRA_REPO="$REPO" \
        SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" SPIRA_INFERENCE_EVERY=0 \
        SPIRA_LAUNCH="$TMP/launch" SPIRA_SYSTEMCTL="$TMP/systemctl" \
        SPIRA_SUMMON="$TMP/launch" \
        SPIRA_SKIP_RECLAIM=1 \
        SPIRA_SKIP_CLOSED_CHECK=1 PATH="$SH:$PATH" \
            command sentinel 2>&1
    )"
    printf '%s\n%s\n' "$audit_out" "$normal_out"
}

# lib.sh under the same configuration, so the two set predicates can be asked directly
# rather than inferred from a pass's output.
predicate() {   # predicate <fn> -> that lib predicate's output under the fixture
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_GOAL=sp-goal \
    SPIRA_FAYTHS="${ROSTER:-t tinc}" \
        bash -c ". \"$SH/lib.sh\"; $1" 2>/dev/null
}
# groomer.sh deadlocked (the git half) into spira-claim deadlocked (the decision and the
# write), under the same configuration as sentinel(), so a deadlock lift is made through the
# real tools against the real fixture repo — not a model of what either would do.
deadlocked() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" \
        bash "$SH/groomer.sh" deadlocked "$@" 2>&1
}
# spira-claim unpoison, same configuration — the operator's own remedy against the real
# store (replaces attempts.sh clear, superseded when unpoison shipped, before this bead).
clearpoison() {
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$REPO" \
    SPIRA_GOAL=sp-goal SPIRA_FAYTHS="${ROSTER:-t tinc}" \
        spira-claim unpoison --bead "$1" --cause "operator judged it worth retrying (test-poison.sh sp-qd2ul)" --db "$SPIRA_DB" 2>&1
}
status_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
assignee_of() { B show "$1" --json 2>/dev/null | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("assignee") or "")'; }
# THE POISON HOLD, READ FROM THE REAL SPIRA-LC (sp-i2m7y) — not the bd label, which CHECK 4
# no longer writes at all.
poisoned()    { lcheld "$1"; }
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
    mklc sp-goal sp-block sp-open
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
    mklc sp-orphan sp-kid sp-young
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
# AND ONLY ON THE TRANSITION. The next pass sees the bead's spira-lc poison hold already
# held and takes the `;;` branch — the spira_event call is never reached.
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
mklc sp-epic
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
    mklc sp-orphan
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
rmpoison sp-orphan
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
# below the threshold must have its spira-lc poison hold released automatically.
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<JSONL
{"id":"sp-stale","title":"stale poison — count below threshold","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
{"id":"sp-live","title":"live poison — count at threshold","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
mkpoison sp-stale sp-live
cycle sp-stale 1
cycle sp-live 3
out="$(SPIRA_POISON_AT=3 sentinel)"
notpoisoned "a poisoned bead with count below threshold has its hold released"  sp-stale
ispoisoned  "a poisoned bead with count at threshold keeps its hold"           sp-live
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
mklc sp-thrash sp-real
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
mklc sp-zero sp-one
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

rmpoison sp-orphan
out="$(sentinel)"
ispoisoned "the bead is poisoned again, because it is still over the threshold" sp-orphan
is "but clearing the hold did NOT re-arm the ask" "1" \
   "$(grep -cE '^send.*Spira bead sp-orphan.*3 attempts' "$MAIL_LOG")"

rmpoison sp-orphan
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
mklc sp-late
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
mklc sp-reclaimed
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
mklc sp-tinc-req
seedn sp-tinc-req reopened '' 5
out="$(SPIRA_REQUEUE_AT=5 sentinel)"
want "the requeue cap fires for the tinc partition's own bead too" \
     "sp-tinc-req — completed and requeued 5 times" "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# sp-wiyr2 — A LIFT BY `spira-claim deadlocked` (via `groomer.sh deadlocked`) SURVIVES THE
# NEXT SENTINEL PASS. deadlocked releases the poison hold on finished, mergeable work; it
# does not touch the attempt count (the rungs are the record of how the bead got here).
# Without the fix CHECK 4 reads that same unchanged count against an unheld bead on its
# very next pass and poisons it right back — the finished-work exemption undone within
# minutes of being granted. (The fix is `mark_poison_lifted`, spira-claim/DESIGN.md §9.)
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
notpoisoned "the hold is off right after the lift" sp-orphan
: > "$MAIL_LOG"; out="$(sentinel)"
notpoisoned "and it is STILL off after the very next sentinel pass" sp-orphan
nowant "no fresh poison ask went out for it either" "sp-orphan" "$(cat "$MAIL_LOG")"
want "the check log shows CHECK 4 actually examined the set" "CHECK4 examining" "$out"

# --------------------------------------------------------------------------------------
# sp-qd2ul — CLEARING THE POISON HOLD MUST STICK. The operator's own remedy (sentinel's ask
# tells a human to run `spira-claim unpoison`) is not the tool-specific `deadlocked` sweep
# above — the bead need not be finished, only judged worth retrying. What is asserted here
# is the property the bare-removal case at line ~399 shows this codebase does NOT get for
# free: a clear survives a sentinel pass with nothing new against it, and is NOT a permanent
# exemption — a genuinely new run of failures after the clear poisons it again.
# --------------------------------------------------------------------------------------
echo
seed_poison; : > "$MAIL_LOG"; out="$(sentinel)"
ispoisoned "sp-orphan is poisoned by the first pass" sp-orphan
cl_out="$(clearpoison sp-orphan)"
want "spira-claim unpoison reports the lift" "OK   sp-orphan: cleared" "$cl_out"
notpoisoned "the hold is off right after the clear" sp-orphan
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

# --------------------------------------------------------------------------------------
# sp-rp4g4 — CHECK 4 FAILS CLOSED WHEN THE BULK QUERY FAILS. check4_bulk_data used to
# discard bd sql's stderr and return no rows on any failure, which every caller then read as
# "0 attempts" for every bead in the set — so the poison/requeue/reclaim caps were silently
# unenforced on exactly the pass where the query broke, with nothing in the log saying why.
# The seam is SPIRA_BD (same as the CHECK 5 case above): forward every call through to the
# real engine except the one bulk query, which is failed on purpose by matching its own
# text — `spira-claim counts` (which the sentinel's CHECK 4 calls) reads the events trail
# with one `bd sql` whose FROM clause is unique to it (spira-claim/src/store.rs events_sql).
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<JSONL
{"id":"sp-failquery","title":"would poison this pass if the bulk query worked","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
mklc sp-failquery
cycle sp-failquery 3   # at POISON_AT=3 — this pass would poison it, if the query ran

REAL_BD_PATH="$(command -v "${SPIRA_BD:-bd}" 2>/dev/null || printf '%s' "${SPIRA_BD:-bd}")"
FAIL_SQL_BD="$TMP/fail-sql-bd"
{
    printf '#!/usr/bin/env bash\n'
    printf 'case "$*" in\n'
    printf '  *"created_at from events where issue_id in ("*) printf "fail-sql-bd: simulated dolt failure\\n" >&2; exit 1 ;;\n'
    printf 'esac\n'
    printf 'exec %q "$@"\n' "$REAL_BD_PATH"
} > "$FAIL_SQL_BD"
chmod +x "$FAIL_SQL_BD"

: > "$MAIL_LOG"
out="$(SPIRA_BD="$FAIL_SQL_BD" sentinel)"
notpoisoned "a bead at the threshold is not poisoned off a query that never ran" sp-failquery
want "the failure is logged by name, not swallowed" \
     "CHECK4 bulk attempts query failed" "$out"
want "and the pass says plainly that it decided nothing" \
     "making no poison/requeue/reclaim decision this pass" "$out"
nowant "no mail went out on the strength of a query that failed" "sp-failquery" "$(cat "$MAIL_LOG")"

# RECOVERY: the failure was in the READ, not the events trail — a normal pass right after
# must poison it exactly as if the failed pass had never happened.
: > "$MAIL_LOG"; out="$(sentinel)"
ispoisoned "the very next (working) pass poisons it as normal" sp-failquery
want "and the operator is asked, same as any other poisoning" "sp-failquery" "$(cat "$MAIL_LOG")"

# --------------------------------------------------------------------------------------
# sp-418h5 — THE STALE-POISON-CLEAR SCAN FAILS CLOSED TOO, on the per-bead attempts_of call
# it makes (distinct from the bulk query sp-rp4g4 fixed above). attempts_of used to return
# '0' on any query error, indistinguishable from a bead that genuinely never failed — so this
# scan cleared a poisoned bead off a query that never ran, and the very next pass's bulk
# query (unaffected by this stub, since a poisoned bead is excluded from dispatchable_open
# only while the label is on) read the bead's real, unchanged count and poisoned it right
# back. Run every 15-60s, that is the exact flip sp-kogm lived through: poisoned 75 times,
# cleared 74, across 175 passes. The seam is SPIRA_BD again: the stale clear counts through
# the same `spira-claim counts` events query as the main loop, so the same text is failed.
# The poison itself is the lifecycle hold (mkpoison), not a bd label.
# --------------------------------------------------------------------------------------
echo
seed; rm -rf "$RUN/poison-asked"
testdb_seed <<JSONL
{"id":"sp-flipstale","title":"would clear this pass if attempts_of's query ran","status":"open","issue_type":"task","labels":["${SPIRA_SCOPE_LABEL}","plan"],"updated_at":"2026-09-04T00:00:00Z"}
JSONL
mkpoison sp-flipstale
cycle sp-flipstale 1   # genuinely below POISON_AT=3 — a real stale poison, if the query ran

FAIL_ATTEMPTS_BD="$TMP/fail-attempts-bd"
{
    printf '#!/usr/bin/env bash\n'
    printf 'case "$*" in\n'
    printf '  *"created_at from events where issue_id in ("*) printf "fail-attempts-bd: simulated dolt failure\\n" >&2; exit 1 ;;\n'
    printf 'esac\n'
    printf 'exec %q "$@"\n' "$REAL_BD_PATH"
} > "$FAIL_ATTEMPTS_BD"
chmod +x "$FAIL_ATTEMPTS_BD"

: > "$MAIL_LOG"
for i in 1 2 3; do
    out="$(SPIRA_BD="$FAIL_ATTEMPTS_BD" SPIRA_POISON_AT=3 sentinel)"
    ispoisoned "pass $i: a stale poison is not cleared off a query that never ran" sp-flipstale
    want "pass $i: the failure is logged by name, not swallowed" \
         "attempts query failed" "$out"
    nowant "pass $i: no clear was reported off the failed query" "stale poison cleared" "$out"
done

# RECOVERY: the failure was in the READ, not the events trail — a normal pass right after
# clears it exactly as if the failed passes had never happened.
out="$(SPIRA_POISON_AT=3 sentinel)"
notpoisoned "the very next (working) pass clears the real stale poison" sp-flipstale
want "and the pass records the clear" "stale poison cleared" "$out"

tl_summary
