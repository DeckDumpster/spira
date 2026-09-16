#!/usr/bin/env bash
#
# test-ask-default.sh — --default is mandatory on add/decide; ask.sh rejected lists premise-rejected
#
#   ./test-ask-default.sh
#
# WHAT THIS GUARDS. A premise-rejected ask proceeds on the default the agent proposed. An ask
# filed without --default cannot be rejected coherently — there is nothing to proceed on. So
# ask.sh add and ask.sh decide must refuse to create a bead unless --default is supplied.
#
# ask.sh rejected is the only training signal the escalation filter has: it lists what the
# operator dismissed as not worth deciding, newest first, with the reason why. This suite
# confirms it renders the right rows and the right close reasons.
#
# defect: sp-d9fe9
# covers: cockpit/ask.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
COCKPIT="$(cd "$HERE/../cockpit" && pwd -P)"
. "$HERE/testdb.sh"
testdb_require test-ask-default
testdb_up ask-default || { echo "testdb_up failed"; exit 1; }

pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$2] got [$3]"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM

export COCKPIT_DB="$SPIRA_DB"
BD="${TESTDB_BD:-bd}"

bdt() { "$BD" -C "$SPIRA_DB" "$@"; }
ask() { COCKPIT_DB="$SPIRA_DB" bash "$COCKPIT/ask.sh" "$@"; }

count_beads() {
    "$BD" -C "$SPIRA_DB" list --all --limit 0 --json 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d if isinstance(d,list) else d.get("issues",[])))' 2>/dev/null || echo 0
}

echo "test-ask-default.sh"

# ======================================================================================
echo
echo "positive control — count_beads can detect a creation"
# ======================================================================================
before="$(count_beads)"
ask insight "positive-control insight" >/dev/null 2>&1 || true
after="$(count_beads)"
[ "$after" -gt "$before" ] \
    && ok "count_beads moves on creation (positive control)" \
    || bad "count_beads moves on creation (positive control)" "before=$before after=$after"

testdb_reset

# ======================================================================================
echo
echo "regression sp-cdcqy: bdt() in ask.sh honours SPIRA_BD"
# ======================================================================================
# ask.sh's bdt() must call \${SPIRA_BD:-bd}, not bare bd.  With bare bd, on an embedded
# fixture, bd may resolve to a binary that cannot open the store — the call hangs, the
# suite watchdog sends SIGTERM to the process group, and ask.sh exits 143.  A spy
# intercepts SPIRA_BD and verifies the create call routes through it.
_spy_dir="$(mktemp -d)"
_spy_log="$_spy_dir/calls"
_spy_bin="$_spy_dir/bd-spy"
_bd_real="$(command -v bd-embedded 2>/dev/null || true)"
if [ -n "$_bd_real" ]; then
    touch "$_spy_log"
    printf '#!/usr/bin/env bash\nprintf '"'"'%%s\n'"'"' "$*" >> %s\nexec %s "$@"\n' \
        "$_spy_log" "$_bd_real" > "$_spy_bin"
    chmod +x "$_spy_bin"
    COCKPIT_DB="$SPIRA_DB" SPIRA_BD="$_spy_bin" \
        bash "$COCKPIT/ask.sh" insight "spy-insight" --why "checking bdt routes" \
        >/dev/null 2>&1 || true
    if grep -q ' create ' "$_spy_log" 2>/dev/null; then
        ok  "bdt() routes create through SPIRA_BD"
    else
        bad "bdt() routes create through SPIRA_BD" \
            "spy not called for create — bdt() may be using bare 'bd'"
    fi
else
    ok "bdt() routes create through SPIRA_BD (skipped: bd-embedded not on PATH)"
fi
rm -rf "$_spy_dir"
testdb_reset

# ======================================================================================
echo
echo "add without --default: refused, non-zero, no bead created"
# ======================================================================================
before="$(count_beads)"
out="$(ask add "should this be allowed" --why "testing" 2>&1)"; rc=$?
after="$(count_beads)"
[ "$rc" -ne 0 ] \
    && ok  "add without --default exits non-zero" \
    || bad "add without --default exits non-zero" "got rc=$rc"
want "add without --default names the reason" "--default" "$out"
is   "add without --default creates no bead"  "$before"  "$after"

# ======================================================================================
echo
echo "decide without --default: refused, non-zero, no bead created"
# ======================================================================================
before="$(count_beads)"
out="$(ask decide "option A or option B" --why "testing" 2>&1)"; rc=$?
after="$(count_beads)"
[ "$rc" -ne 0 ] \
    && ok  "decide without --default exits non-zero" \
    || bad "decide without --default exits non-zero" "got rc=$rc"
want "decide without --default names the reason" "--default" "$out"
is   "decide without --default creates no bead"  "$before"  "$after"

# ======================================================================================
echo
echo "add WITH --default: succeeds, creates a bead"
# ======================================================================================
before="$(count_beads)"
out="$(ask add "should we use method A" --default "use method A" --why "testing" 2>&1)"; rc=$?
after="$(count_beads)"
is  "add with --default exits 0"        "0"       "$rc"
[ "$after" -gt "$before" ] \
    && ok  "add with --default creates a bead" \
    || bad "add with --default creates a bead" "before=$before after=$after"

# ======================================================================================
echo
echo "decide WITH --default: succeeds, creates a bead"
# ======================================================================================
before="$(count_beads)"
out="$(ask decide "option A or option B" --default "option A" --why "testing" 2>&1)"; rc=$?
after="$(count_beads)"
is  "decide with --default exits 0"        "0"      "$rc"
[ "$after" -gt "$before" ] \
    && ok  "decide with --default creates a bead" \
    || bad "decide with --default creates a bead" "before=$before after=$after"

# ======================================================================================
echo
echo "insight without --default: NOT affected — must succeed"
# ======================================================================================
before="$(count_beads)"
out="$(ask insight "something was learned here" --why "it matters" 2>&1)"; rc=$?
after="$(count_beads)"
is  "insight without --default exits 0"        "0"       "$rc"
[ "$after" -gt "$before" ] \
    && ok  "insight without --default creates a bead" \
    || bad "insight without --default creates a bead" "before=$before after=$after"

# ======================================================================================
echo
echo "ask.sh rejected: lists premise-rejected asks newest first with reasons"
# ======================================================================================

# Create two ask-question beads and premise-reject them with different reasons.
# Output format: "asked [sp-xxx] first rejected question" — extract the id from brackets.
id1="$(ask add "first rejected question" --default "do the first thing" --why "because" 2>&1 | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
id2="$(ask add "second rejected question" --default "do the second thing" --why "because" 2>&1 | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"

# Premise-reject both using the canonical label and close reason format the panel uses.
if [ -n "$id1" ] && [ -n "$id2" ]; then
    bdt label add "$id1" "premise-rejected" >/dev/null 2>&1 || true
    bdt close "$id1" --reason "premise-rejected: the agent misunderstood the scope" --force >/dev/null 2>&1 || true
    bdt label add "$id2" "premise-rejected" >/dev/null 2>&1 || true
    bdt close "$id2" --reason "premise-rejected: not our decision to make" --force >/dev/null 2>&1 || true

    rejected="$(ask rejected 2>&1)"
    want "rejected shows first bead title"           "first rejected question"          "$rejected"
    want "rejected shows second bead title"          "second rejected question"         "$rejected"
    want "rejected shows first bead's why"           "the agent misunderstood the scope" "$rejected"
    want "rejected shows second bead's why"          "not our decision to make"          "$rejected"
    want "rejected shows count"                      "premise-rejected"                  "$rejected"
else
    bad "rejected: could not create test beads (id1=$id1 id2=$id2)" "bead creation failed"
    bad "rejected shows first bead title"  "skipped" "bead creation failed"
    bad "rejected shows second bead title" "skipped" "bead creation failed"
    bad "rejected shows first bead's why"  "skipped" "bead creation failed"
    bad "rejected shows second bead's why" "skipped" "bead creation failed"
    bad "rejected shows count"             "skipped" "bead creation failed"
fi

# ======================================================================================
echo
echo "--priority sets the bead's priority; the unflagged default is unchanged"
# ======================================================================================
prio_of() { "$BD" -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); d=d[0] if isinstance(d,list) else d; print(d.get("priority",""))' 2>/dev/null; }
pid="$(ask add "priority-flagged question" --default "d" --priority 3 2>&1 | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
did="$(ask add "priority-unflagged question" --default "d" 2>&1 | grep -oE '\[[a-z]+-[a-z0-9]+\]' | tr -d '[]' | head -1)"
is "--priority 3 reaches the bead" "3" "$(prio_of "$pid")"
is "no flag keeps P1"              "1" "$(prio_of "$did")"
before="$(count_beads)"
out="$(ask add "bad priority question" --default "d" --priority 7 2>&1)"; rc=$?
[ "$rc" -ne 0 ] && ok "--priority 7 is refused" || bad "--priority 7 is refused" "rc=$rc"
is "a refused priority files nothing" "$before" "$(count_beads)"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
