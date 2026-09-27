#!/usr/bin/env bash
# test-answers.sh — answers.py verdict/premise rendering, cursor seeding, and the watcher loop.
#
# Merges test-answers-premise-rejected.sh and test-answers-silent-seed.sh (docs/test-plan/
# operator-channel.md rows 20-23, cluster D5) onto fixture bead JSON instead of a testdb: both
# built their own embedded-Dolt instance to create and close a handful of beads whose shape
# answers.py never distinguishes from a hand-written fixture. What answers.py DOES need from a
# real bd is `history --events` for the closing actor, which the embedded binary's own version
# does not support either way — every existing suite here already stubs that one call.
#
# THE SCAR (sp-kw9bo). A premise-rejected close (Ryan declining to be asked, not answering)
# rendered as "RYAN ANSWERED" on 2026-09-10, and a session acted on four dismissals as
# affirmations. detection uses a label OR a reason prefix (answers.py:268-269); G-06 requires
# each covered on its own, not only together as the scar fixture originally did.
#
# tier: T1
# covers: spira/answers.py spira/watchd.sh cockpit/watch-answers.sh UC-operator-channel-20 UC-operator-channel-21 UC-operator-channel-22 UC-operator-channel-23
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
isz() { [ "$2" = 0 ] && ok "$1" || bad "$1" "wanted exit 0 got $2"; }

echo "test-answers.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

ASK_LABEL="needs-attention"   # pinned to a non-default (law-gates-run-in-a-clean-environment)
OPERATOR_ACTOR="testop"

# THE STUB. answers.py's closed_actors() issues one `bd sql --json 'SELECT ... FROM events
# ...'` batched over every candidate id, which no fixture JSON can answer (it comes from the
# audit trail, not the issue row). Every id named in the query's IN (...) is treated as closed
# by the operator — that is the fact every fixture row below is built to assert about.
STUB_BD="$TMP/bd"
cat > "$STUB_BD" <<STUBEOF
#!/usr/bin/env bash
for arg; do
    case "\$arg" in
        *"FROM events"*)
            printf '['
            first=1
            for id in \$(printf '%s' "\$arg" | grep -oE "'[^']+'" | tr -d "'"); do
                [ "\$first" = 1 ] || printf ','
                first=0
                printf '{"issue_id":"%s","actor":"%s","created_at":"2026-09-10T00:00:00Z"}' \
                    "\$id" "$OPERATOR_ACTOR"
            done
            printf ']'
            exit 0
            ;;
    esac
done
echo '[]'
STUBEOF
chmod +x "$STUB_BD"

touch "$TMP/self-closed"

reset_marks() {
    printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/vmark"
    printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/cmark"
}

run_answers() {   # run_answers <format> <fixture-json> -> stdout+stderr
    reset_marks
    printf '%s' "$2" | python3 "$HERE/answers.py" \
        "bd=$STUB_BD" "db=unused" \
        "ask_label=$ASK_LABEL" \
        "operator_actor=$OPERATOR_ACTOR" \
        "operator=ryan" \
        "verdict_cursor=$TMP/vmark" "comment_cursor=$TMP/cmark" \
        "self_closed=$TMP/self-closed" \
        ${ANSWERS_NOW:+"now=$ANSWERS_NOW"} \
        "format=$1" 2>&1
}

bead_row() {   # bead_row <id> <closed_at> <extra-labels-csv-or-empty> <reason>
    local id="$1" closed_at="$2" extra="$3" reason="$4" labels="\"$ASK_LABEL\",\"overseer\""
    [ -n "$extra" ] && labels="$labels,\"$extra\""
    printf '{"id":"%s","title":"t %s","status":"closed","issue_type":"decision","labels":[%s],"closed_at":"%s","close_reason":"%s","comment_count":0}' \
        "$id" "$id" "$labels" "$closed_at" "$reason"
}

VID="sp-ta-verdict"
LID="sp-ta-label-only"
PID="sp-ta-prefix-only"
FIXTURE="[$(bead_row "$VID" "2026-09-10T00:00:00Z" "" "yes, proceed with plan X"),$(bead_row "$LID" "2026-09-10T00:01:00Z" "premise-rejected" "not my call to make, proceed on default"),$(bead_row "$PID" "2026-09-10T00:02:00Z" "" "premise-rejected: not this session's call, proceed on default")]"

# ==========================================================================
# UC-20 — positive control, then the two premise-rejected detection branches
# independently (G-06): label-only, then prefix-only.
# ==========================================================================
echo
echo "UC-20: positive control — every fixture bead is seen and reported"

out_mon="$(run_answers monitor "$FIXTURE")"
out_ses="$(run_answers session "$FIXTURE")"
want "verdict bead appears in monitor output"   "$VID" "$out_mon"
want "verdict bead appears in session output"   "$VID" "$out_ses"
want "label-only bead appears in monitor output" "$LID" "$out_mon"
want "prefix-only bead appears in monitor output" "$PID" "$out_mon"

echo
echo "UC-20/G-06: label-only premise rejection (no reason prefix) is detected on its own"

nowant "monitor: label-only bead is not RYAN ANSWERED" "RYAN ANSWERED $LID" "$out_mon"
want   "monitor: label-only bead is RYAN REJECTED THE PREMISE" "RYAN REJECTED THE PREMISE $LID" "$out_mon"
nowant "session: label-only bead is not 'verdict on'" "verdict on \`$LID\`" "$out_ses"
want   "session: label-only bead carries PREMISE REJECTED" "PREMISE REJECTED on \`$LID\`" "$out_ses"

echo
echo "UC-20/G-06: prefix-only premise rejection (no label) is detected on its own"

nowant "monitor: prefix-only bead is not RYAN ANSWERED" "RYAN ANSWERED $PID" "$out_mon"
want   "monitor: prefix-only bead is RYAN REJECTED THE PREMISE" "RYAN REJECTED THE PREMISE $PID" "$out_mon"
want   "the reason is carried without its prefix" "not this session's call" "$out_mon"

echo
echo "UC-20: a regular verdict still renders as RYAN ANSWERED (no regression)"

want "monitor: verdict bead is RYAN ANSWERED" "RYAN ANSWERED $VID" "$out_mon"
want "session: verdict bead is verdict on"     "verdict on \`$VID\`" "$out_ses"

# ==========================================================================
# UC-21 — every monitor headline carries the bead's own closed_at, in the
# format watchd.sh parses. SOURCE-GREP: the matcher is lifted out of
# watchd.sh, not restated, so the two sides cannot drift apart silently.
# ==========================================================================
echo
echo "UC-21: monitor headline stamp is the bead's own clock, in watchd's own shape"

STAMP_RE="$(sed -n "s/^[[:space:]]*| sed -n '\(.*\)' | sort | head -1)\"$/\1/p" "$HERE/watchd.sh")"
if [ -z "$STAMP_RE" ]; then
    bad "watchd.sh's own stamp matcher was located" \
        "no stamp-extracting sed in watchd.sh — the contract below cannot be checked"
else
    ok "watchd.sh's own stamp matcher was located"
    # PINNED AWAY FROM THE BEADS' OWN CLOSED_AT so a stamp reading the pass clock would fail.
    out_stamped="$(ANSWERS_NOW=2001-01-01T00:00:00Z run_answers monitor "$FIXTURE")"
    # Parallel arrays, not a colon-packed string: an ISO8601 timestamp carries colons of its
    # own, so encoding one as a colon-delimited field truncates it at "HH".
    _whats=(verdict rejection)
    _ids=("$VID" "$PID")
    _closed_ats=("2026-09-10T00:00:00Z" "2026-09-10T00:02:00Z")
    _greps=("ANSWERED $VID" "REJECTED THE PREMISE $PID")
    for _i in 0 1; do
        _line="$(printf '%s\n' "$out_stamped" | grep -F "${_greps[$_i]}" | head -1)"
        _stamp="$(printf '%s\n' "$_line" | sed -n "$STAMP_RE")"
        is "the ${_whats[$_i]} headline's stamp is ${_ids[$_i]}'s own closed_at" "${_closed_ats[$_i]}" "$_stamp"
        nowant "the ${_whats[$_i]} stamp is the pass's clock, not the bead's" "2001-01-01" "$_stamp"
    done
fi

# ==========================================================================
# UC-22 — cursor seeding: cold start is silent, recovery reports SEEDED AT
# with a count, a normal pass with advanced marks is silent again.
# ==========================================================================
echo
echo "UC-22: cursor seeding — cold start, crash recovery, normal pass"

AID="sp-ta-seed"
SEED_FIXTURE="[$(bead_row "$AID" "2026-09-10T00:00:00Z" "" "yes, proceed")]"
WITNESS="$TMP/witness.txt"

run_seed_answers() {
    printf '%s' "$SEED_FIXTURE" | python3 "$HERE/answers.py" \
        "bd=$STUB_BD" "db=unused" \
        "ask_label=$ASK_LABEL" "operator_actor=$OPERATOR_ACTOR" "operator=testop" \
        "verdict_cursor=$TMP/vmark" "comment_cursor=$TMP/cmark" \
        "witness=$WITNESS" "self_closed=$TMP/self-closed" "format=monitor" 2>&1
}

rm -f "$WITNESS" "$TMP/vmark" "$TMP/cmark"
out="$(run_seed_answers)"
nowant "cold start (no witness, no marks) produces no SEEDED line" "SEEDED" "$out"
[ -f "$TMP/vmark" ] && ok "cold start wrote a cursor (positive control: it ran)" \
    || bad "cold start wrote a cursor (positive control)" "cursor not created"

rm -f "$TMP/vmark" "$TMP/cmark"
printf '%s\n' "$AID" > "$WITNESS"
out="$(run_seed_answers)"
want "recovery (witness present, marks absent) emits SEEDED AT" "SEEDED AT" "$out"
want "SEEDED line reports 1 waiting answer" "1 answer" "$out"

printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/vmark"
printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/cmark"
printf '%s\n' "$AID" > "$WITNESS"
out="$(run_seed_answers)"
nowant "normal pass with advanced marks produces no SEEDED line" "SEEDED" "$out"

# ==========================================================================
# UC-23 — watch-answers.sh loop: at-least-once-until-read (V10, sp-12uu8), NOT
# exactly-once (docs/test-plan/operator-channel.md's own UC-23 text predates
# the review and is superseded by it — see this bead's close reason). Runs
# against a stub answers.py (ANSWERS_BIN), so the loop's own contract — does
# it print, does it wake, does it exit 1 on a failing pass — is exercised
# without a real bd round trip.
# ==========================================================================
echo
echo "UC-23: watch-answers.sh loop"

FAKE_COCKPIT_DB="$TMP/cockpitdb"; mkdir -p "$FAKE_COCKPIT_DB/.beads"
STUB_BD_ROWS="$TMP/bd-rows"
cat > "$STUB_BD_ROWS" <<'STUBEOF'
#!/usr/bin/env bash
echo '[]'
STUBEOF
chmod +x "$STUB_BD_ROWS"

STUB_ANSWERS="$TMP/stub-answers.sh"
cat > "$STUB_ANSWERS" <<'STUBEOF'
#!/usr/bin/env bash
cat >/dev/null
[ -f "${STUB_ANSWERS_FAIL:-}" ] && exit 1
[ -f "${STUB_ANSWERS_OUT:-}" ] && cat "$STUB_ANSWERS_OUT"
exit 0
STUBEOF
chmod +x "$STUB_ANSWERS"

export WAKES="$TMP/wakes"
cat > "$TMP/wake" <<'W'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "$WAKES"
W
chmod +x "$TMP/wake"

run_watch_loop() {   # run_watch_loop <wake-cmd> <run-dir> -> loop stdout on stdout, exit is the loop's
    timeout 5 env \
        COCKPIT_DB="$FAKE_COCKPIT_DB" BD_BIN="$STUB_BD_ROWS" \
        SPIRA_ASK_LABEL="$ASK_LABEL" SPIRA_RUN="$2" \
        ANSWER_STATE="$2/witness" VERDICT_CURSOR="$2/.vc" COMMENT_CURSOR="$2/.cc" \
        SELF_CLOSED="$TMP/self-closed" ANSWERS_BIN="$STUB_ANSWERS" ANSWER_POLL=1 SPIRA_WAKE="$1" \
        bash "$HERE/../cockpit/watch-answers.sh" loop 2>/dev/null
}

echo "sp-ta-loop THE OPERATOR ANSWERED sp-ta-loop — stub output" > "$TMP/answers-out"
export STUB_ANSWERS_OUT="$TMP/answers-out"

: > "$WAKES"
out_loop="$(run_watch_loop "$TMP/wake" "$TMP/wrun1")"
want "the loop printed the stub's output" "ANSWERED" "$out_loop"
wn="$(grep -c 'drain answers' "$WAKES" 2>/dev/null)" || wn=0
[ "$wn" -ge 1 ] && ok "at-least-once: the loop woke the reader (V10 — not asserted as exactly once)" \
    || bad "at-least-once: the loop woke the reader" "got $wn wakes"

: > "$WAKES"
run_watch_loop "" "$TMP/wrun2" >/dev/null
is "an empty SPIRA_WAKE wakes nobody" "" "$(cat "$WAKES")"

echo
echo "UC-23: the loop exits 1 when a pass fails (so systemd restarts it)"

touch "$TMP/answers-fail"
export STUB_ANSWERS_FAIL="$TMP/answers-fail"
loop_rc=0
run_watch_loop "" "$TMP/wrun3" >/dev/null 2>&1 || loop_rc=$?
[ "$loop_rc" -eq 1 ] \
    && ok "watch-answers.sh loop exits 1 when a pass fails" \
    || bad "watch-answers.sh loop exits 1 when a pass fails" \
           "got exit $loop_rc (0=stub did not fail, 124=looped forever)"

echo
echo "UC-23: a failed wake leaves the cursor unadvanced, so the pass is re-announced (V10, sp-12uu8, adapted to a cursor design)"

# A stub that behaves like answers.py's own cursor write: it advances both marks on every
# call, whether or not the wake that follows will succeed. _watch_pass is sourced directly
# (rather than run through `loop` under a `timeout`) so the cursor files can be asserted on
# between passes without racing a kill signal against the rollback.
STUB_ANSWERS_CURSOR="$TMP/stub-answers-cursor.sh"
cat > "$STUB_ANSWERS_CURSOR" <<'STUBEOF'
#!/usr/bin/env bash
cat >/dev/null
echo "cursor-advanced-$$" > "$VERDICT_CURSOR"
echo "cursor-advanced-$$" > "$COMMENT_CURSOR"
echo "sp-ta-loop THE OPERATOR ANSWERED sp-ta-loop — stub output"
STUBEOF
chmod +x "$STUB_ANSWERS_CURSOR"

FAILWAKE="$TMP/wake-fail.sh"
cat > "$FAILWAKE" <<'W'
#!/usr/bin/env bash
exit 1
W
chmod +x "$FAILWAKE"

RUNDIR="$TMP/wrun-rollback"; mkdir -p "$RUNDIR"
printf 'seed-verdict\n' > "$RUNDIR/.vc"
printf 'seed-comment\n' > "$RUNDIR/.cc"

run_watch_pass() {   # run_watch_pass <wake-cmd> -> _watch_pass's stdout
    env \
        COCKPIT_DB="$FAKE_COCKPIT_DB" BD_BIN="$STUB_BD_ROWS" \
        SPIRA_ASK_LABEL="$ASK_LABEL" SPIRA_RUN="$RUNDIR" \
        ANSWER_STATE="$RUNDIR/witness" VERDICT_CURSOR="$RUNDIR/.vc" COMMENT_CURSOR="$RUNDIR/.cc" \
        SELF_CLOSED="$TMP/self-closed" ANSWERS_BIN="$STUB_ANSWERS_CURSOR" SPIRA_WAKE="$1" \
        bash -c '. "$1"; _watch_pass' _ "$HERE/../cockpit/watch-answers.sh"
}

out="$(run_watch_pass "$FAILWAKE" 2>/dev/null)"
want "positive control: the pass still printed the stub's answer" "ANSWERED" "$out"
is "a failed wake leaves VERDICT_CURSOR unadvanced" "seed-verdict" "$(cat "$RUNDIR/.vc")"
is "a failed wake leaves COMMENT_CURSOR unadvanced" "seed-comment" "$(cat "$RUNDIR/.cc")"

out2="$(run_watch_pass "$FAILWAKE" 2>/dev/null)"
want "wake still failing: the next pass re-announces the same answer" "ANSWERED" "$out2"
is "wake still failing: VERDICT_CURSOR is still unadvanced" "seed-verdict" "$(cat "$RUNDIR/.vc")"

out3="$(run_watch_pass "$TMP/wake" 2>/dev/null)"
want "a succeeding wake still prints the answer" "ANSWERED" "$out3"
vc_after="$(cat "$RUNDIR/.vc")"
[ "$vc_after" != "seed-verdict" ] \
    && ok "a succeeding wake lets VERDICT_CURSOR advance" \
    || bad "a succeeding wake lets VERDICT_CURSOR advance" "cursor still at seed value"

# ==========================================================================
# UC-24 (sp-xsl8i) — the fan-out is gone: hundreds of candidates on each leg
# cost a FIXED number of bd calls, not one per bead. A stub that logs every
# invocation it receives is the only way to assert a call COUNT rather than
# wall time, which is what the bead requires.
# ==========================================================================
echo
echo "UC-24: hundreds of candidates cost a bounded number of bd calls, not one per bead"

N=300
CALLS="$TMP/calls.log"
STUB_BD_BOUNDED="$TMP/bd-bounded"
cat > "$STUB_BD_BOUNDED" <<STUBEOF
#!/usr/bin/env bash
echo "\$*" >> "$CALLS"
for arg; do
    case "\$arg" in
        *"FROM events"*|*"FROM comments"*)
            table=events; case "\$arg" in *"FROM comments"*) table=comments;; esac
            printf '['
            first=1
            for id in \$(printf '%s' "\$arg" | grep -oE "'[^']+'" | tr -d "'"); do
                [ "\$first" = 1 ] || printf ','
                first=0
                if [ "\$table" = events ]; then
                    printf '{"issue_id":"%s","actor":"%s","created_at":"2026-09-10T00:00:00Z"}' \
                        "\$id" "$OPERATOR_ACTOR"
                else
                    printf '{"issue_id":"%s","id":"c-%s","author":"%s","text":"ack","created_at":"2026-09-10T00:00:00Z"}' \
                        "\$id" "\$id" "$OPERATOR_ACTOR"
                fi
            done
            printf ']'
            exit 0
            ;;
    esac
done
echo '[]'
STUBEOF
chmod +x "$STUB_BD_BOUNDED"

rows=""
for i in $(seq 1 "$N"); do
    rows="$rows$(bead_row "sp-ta-v$i" "2026-09-10T00:00:00Z" "" "verdict $i"),"
done
for i in $(seq 1 "$N"); do
    rows="${rows}{\"id\":\"sp-ta-c$i\",\"title\":\"c $i\",\"status\":\"open\",\"labels\":[\"insight\"],\"comment_count\":1},"
done
BIG_FIXTURE="[${rows%,}]"

: > "$CALLS"
printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/vmark-big"
printf '{"ts":"2020-01-01T00:00:00Z","seen":[]}\n' > "$TMP/cmark-big"
out_big=$(printf '%s' "$BIG_FIXTURE" | python3 "$HERE/answers.py" \
    "bd=$STUB_BD_BOUNDED" "db=unused" \
    "ask_label=$ASK_LABEL" "operator_actor=$OPERATOR_ACTOR" "operator=ryan" \
    "verdict_cursor=$TMP/vmark-big" "comment_cursor=$TMP/cmark-big" \
    "self_closed=$TMP/self-closed" "format=monitor" 2>&1)

ncalls=$(grep -c . "$CALLS" 2>/dev/null || echo 0)
[ "${ncalls:-0}" -le 4 ] \
    && ok "answers.py issues a bounded number of bd calls for $((N * 2)) candidates (got $ncalls)" \
    || bad "answers.py issues a bounded number of bd calls for $((N * 2)) candidates" \
           "got $ncalls calls, wanted <= 4"

want "positive control: a verdict candidate still gets reported" "ANSWERED sp-ta-v1 " "$out_big"
want "positive control: a comment candidate still gets reported" "COMMENTED ON sp-ta-c1 " "$out_big"

tl_summary
