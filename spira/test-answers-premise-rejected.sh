#!/usr/bin/env bash
#
# test-answers-premise-rejected.sh — a premise-rejected close must never render as RYAN ANSWERED.
#
# THE SCAR. answered-since.sh announced four dismissals as "RYAN ANSWERED ... done" on
# 2026-09-10 and a session acted on them as affirmations, enacting statutes and verifying
# "fixes" for things Ryan had refused to be asked about. The bead's close_reason carried the
# prefix `premise-rejected:`, which answers.py did not distinguish from a verdict.
#
# WHAT IS UNDER TEST. answers.py is the single implementation behind both answered-since.sh
# (format=session) and watch-answers.sh (format=monitor). Both format paths render verdicts
# and must render rejections distinctly.
#
# EVERY CASE IS A PAIR (law-absence-needs-a-positive-control): the rejection is asserted
# never to produce RYAN ANSWERED beside a regular verdict that is asserted still to. A check
# asserting only absence passes just as well against a broken answers.py that produces nothing.
#
# THE BD STUB. answers.py's closed_by() calls `bd history <id> --events --json`, which the
# embedded binary (bd-embedded 1.2.2) does not support. A stub wraps bd-embedded, handles
# only the history-events call, and delegates everything else — so all database operations
# run against the real throwaway instance (law-prefer-the-real-dependency). What is stubbed
# is a feature the embedded binary lacks for version reasons, not a behaviour we are trying to
# avoid testing.
#
# defect: sp-kw9bo
# covers: spira/answers.py spira/watchd.sh cockpit/watch-answers.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testdb.sh"
testdb_require test-answers-premise-rejected
testdb_up answers-premise-rejected || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

echo "test-answers-premise-rejected.sh"

# The actor configured as the operator.
OPERATOR_ACTOR="testop"
# The ask label the attention-surface query filters on.
ASK_LABEL="needs-operator"

bdc() { BEADS_ACTOR="$OPERATOR_ACTOR" "$SPIRA_BD" -C "$SPIRA_DB" "$@" 2>/dev/null; }

# A regular verdict bead: closed with an affirmative reason, no premise-rejected prefix/label.
VID=$(bdc create --title "should I proceed with plan X?" --type decision \
    -l "${ASK_LABEL},overseer" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("id",""))' \
    2>/dev/null) || VID=""
bdc close "$VID" --reason "yes, proceed with plan X" >/dev/null 2>&1 || true

# A premise-rejected bead: closed with the `premise-rejected:` prefix, and the label added.
# Both are set as the design specifies: the prefix is what a human or a model reads first,
# the label is what a query can filter on (escalation-taxonomy-2026-09-09.md §1).
RID=$(bdc create --title "was this the right call entirely?" --type decision \
    -l "${ASK_LABEL},overseer" --json 2>/dev/null \
    | sed -n '/^[[{]/,$p' \
    | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("id",""))' \
    2>/dev/null) || RID=""
bdc close "$RID" --reason "premise-rejected: not my call to make, proceed on default" >/dev/null 2>&1 || true
bdc label add "$RID" premise-rejected >/dev/null 2>&1 || true

if [ -z "$VID" ] || [ -z "$RID" ]; then
    echo "FAIL: could not create fixture beads (VID=$VID RID=$RID)"
    exit 1
fi

# THE STUB. bd-embedded (1.2.2) does not support `history --events`, which closed_by()
# requires. The stub intercepts that one subcommand and returns a synthetic event recording
# the operator actor, letting every other call reach bd-embedded unchanged. What is stubbed is
# a version gap, not a behaviour to avoid testing; the rendering logic under test is orthogonal
# to actor attribution.
STUB_BD="$TMP/bd"
cat > "$STUB_BD" <<STUBEOF
#!/usr/bin/env bash
# history <id> --events → return a synthetic operator close event
for arg; do
    case "\$arg" in --events) exec python3 -c "import sys; print('{\"events\":[{\"event_type\":\"closed\",\"actor\":\"$OPERATOR_ACTOR\",\"created_at\":\"2026-09-10T00:00:00Z\"}]}')" ;;  esac
done
exec "$SPIRA_BD" "\$@"
STUBEOF
chmod +x "$STUB_BD"

# The bead list answers.py reads on stdin: the same subset cockpit_attention_beads returns.
BEAD_JSON=$("$SPIRA_BD" -C "$SPIRA_DB" list --all --limit 0 \
    --label-any "${ASK_LABEL},overseer" --json 2>/dev/null | sed -n '/^[[{]/,$p')

# Cursor files seeded to a past timestamp so every close in the fixture is "new".
# A fresh (empty) mark causes answers.py to seed at `now` and report nothing — the first-run
# rule exists to avoid replaying history, not to suppress a test.
reset_marks() {
    printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/vmark"
    printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/cmark"
}

# The self_closed file: neither bead was closed by the harness.
touch "$TMP/self-closed"

run_answers() {  # run_answers <format> → stdout+stderr
    reset_marks
    printf '%s' "$BEAD_JSON" | python3 "$HERE/answers.py" \
        "bd=$STUB_BD" "db=$SPIRA_DB" \
        "ask_label=$ASK_LABEL" \
        "operator_actor=$OPERATOR_ACTOR" \
        "operator=ryan" \
        "verdict_cursor=$TMP/vmark" "comment_cursor=$TMP/cmark" \
        "self_closed=$TMP/self-closed" \
        ${ANSWERS_NOW:+"now=$ANSWERS_NOW"} \
        "format=$1" 2>&1
}

echo
echo "positive control — both beads are seen and reported (stub delivers the operator actor)"
out_mon="$(run_answers monitor)"
out_ses="$(run_answers session)"
want "the verdict bead appears in monitor output" "$VID" "$out_mon"
want "the verdict bead appears in session output" "$VID" "$out_ses"
want "the rejection bead appears in monitor output" "$RID" "$out_mon"
want "the rejection bead appears in session output" "$RID" "$out_ses"

echo
echo "CORE: a premise-rejected bead never produces RYAN ANSWERED (the scar)"
nowant "monitor: premise-rejected bead is not RYAN ANSWERED" \
    "RYAN ANSWERED $RID" "$out_mon"
nowant "session: premise-rejected bead is not verdict on" \
    "verdict on \`$RID\`" "$out_ses"

echo
echo "CORE: a regular verdict still renders as RYAN ANSWERED (no regression)"
want "monitor: verdict bead is RYAN ANSWERED" \
    "RYAN ANSWERED $VID" "$out_mon"
want "session: verdict bead is verdict on" \
    "verdict on \`$VID\`" "$out_ses"

echo
echo "rejection renders distinctly — the reader knows to proceed on the stated default"
want "monitor: rejection renders as RYAN REJECTED THE PREMISE" \
    "RYAN REJECTED THE PREMISE $RID" "$out_mon"
want "session: rejection carries a PREMISE REJECTED marker" \
    "PREMISE REJECTED" "$out_ses"

echo
echo "the close reason is carried so the agent knows WHY (not just THAT)"
want "monitor: rejection reason is in the output" \
    "not my call to make" "$out_mon"
want "session: rejection reason is in the output" \
    "not my call to make" "$out_ses"

echo
echo "every monitor headline carries the answer's own clock, in the shape watchd parses"
# `watchd.sh notify` dates an unread backlog from this stamp; without it, answers written
# all at once when a dead watcher recovers all look new and get a grace period already spent.
# The two sides are one contract, so the READING side's expression is lifted out of watchd.sh
# and run here rather than restated — a copy would let them drift apart silently.
#
# NOTHING BELOW RUNS IF THAT LIFT FAILS. An empty expression falls back to `sed -n p`, which
# echoes the whole line, and every assertion here then passes against output carrying no
# stamp at all — seen, on the first version of this block.
closed_at() {
    bdc show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' \
        | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("closed_at",""))' 2>/dev/null
}
STAMP_RE="$(sed -n "s/^[[:space:]]*| sed -n '\(.*\)' | sort | head -1)\"$/\1/p" "$HERE/watchd.sh")"
if [ -z "$STAMP_RE" ]; then
    bad "watchd.sh's own stamp matcher was located" \
        "no stamp-extracting sed in watchd.sh — the contract below cannot be checked"
else
    ok "watchd.sh's own stamp matcher was located"
    # PINNED TO A TIME THESE BEADS WERE NOT CLOSED AT. Otherwise the pass clock and the close
    # clock share a second and a stamp reading `now` satisfies everything here.
    out_stamped="$(ANSWERS_NOW=2001-01-01T00:00:00Z run_answers monitor)"
    for _pair in "verdict:$VID:ANSWERED $VID" "rejection:$RID:REJECTED THE PREMISE $RID"; do
        _what="${_pair%%:*}"; _rest="${_pair#*:}"; _id="${_rest%%:*}"; _grep="${_rest#*:}"
        _line="$(printf '%s\n' "$out_stamped" | grep -F "$_grep" | head -1)"
        _stamp="$(printf '%s\n' "$_line" | sed -n "$STAMP_RE")"
        is "the $_what headline's stamp is the bead's close time, and watchd reads it" \
           "$(closed_at "$_id")" "$_stamp"
        nowant "the $_what stamp is the bead's clock, not the pass's" "2001-01-01" "$_stamp"
    done
fi

echo
echo "the watcher wakes the reader once per pass that found answers"
WAKES="$TMP/wakes"; : > "$WAKES"
cat > "$TMP/wake" <<W
#!/usr/bin/env bash
printf '%s\n' "\$1" >> "$WAKES"
W
chmod +x "$TMP/wake"
run_watcher() {  # run_watcher <wake-cmd> → watcher stdout
    reset_marks
    timeout 6 env SPIRA_DB="$SPIRA_DB" COCKPIT_DB="$SPIRA_DB" BD_BIN="$STUB_BD" \
        SPIRA_RUN="$TMP/wrun" SPIRA_ASK_LABEL="$ASK_LABEL" SPIRA_OPERATOR_ACTOR="$OPERATOR_ACTOR" \
        ANSWER_STATE="$TMP/wrun/witness" VERDICT_CURSOR="$TMP/vmark" COMMENT_CURSOR="$TMP/cmark" \
        SELF_CLOSED="$TMP/self-closed" ANSWER_POLL=1 SPIRA_WAKE="$1" \
        bash "$HERE/../cockpit/watch-answers.sh" loop 2>/dev/null
}
wout="$(run_watcher "$TMP/wake")"
want "the watcher printed the verdict"            "ANSWERED" "$wout"
wn="$(grep -c 'drain answers' "$WAKES" 2>/dev/null)" || wn=0
is   "it woke the reader exactly once"            "1" "$wn"
: > "$WAKES"
run_watcher "" >/dev/null
is   "an empty SPIRA_WAKE wakes nobody"           "" "$(cat "$WAKES")"

echo
printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
