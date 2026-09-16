#!/usr/bin/env bash
#
# test-concierge.sh — the Concierge persona: composed like an aeon, summoned by nobody.
#
#   ./test-concierge.sh
#
# Two claims, and the second is the one with teeth.
#
# COMPOSED — concierge.sh renders chamber/concierge.md with every placeholder substituted and
# the statute book appended in full, and REFUSES to start when it cannot. A concierge launched
# without its brief looks identical to a working one from outside, and the way anybody finds
# out is the next violated statute.
#
# NEVER SUMMONED — the sentinel draws from spira_task_fayths and spira_lane_fayths, and
# neither may ever contain it. This one carries its own positive control: an ordinary fayth
# sits in the same fixture chamber and MUST appear, because a test asserting only absence
# passes just as well against a roster that is empty for some unrelated reason
# (law-absence-needs-a-positive-control).
#
# No database for the roster half, no network, under a second.
#
# defect: sp-u4x
# covers: spira/lib.sh concierge.sh spira/bead.sh spira/chamber/concierge.fayth spira/chamber/concierge.md
# hermetic-ok: fixture chamber, no systemd or database for the roster checks
# requires: claude
# host-reason: the brief section invokes concierge.sh which requires claude and tmux on PATH (operator tools not available in the container)
# scar: the concierge persona lacked FAYTH_SUMMON=operator and could be claimed by the sentinel as an ordinary worker.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HARNESS="$(cd "$HERE/.." && pwd)"
pass=0; fail=0
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

is()     { if [ "$2" = "$3" ]; then pass=$((pass+1)); printf '  ok    %s\n' "$1"
           else fail=$((fail+1)); printf '  FAIL  %s: want [%s] got [%s]\n' "$1" "$2" "$3"; fi; }
want()   { case "$3" in *"$2"*) pass=$((pass+1)); printf '  ok    %s\n' "$1" ;;
           *) fail=$((fail+1)); printf '  FAIL  %s: wanted [%s]\n' "$1" "$2" ;; esac; }
nowant() { case "$3" in *"$2"*) fail=$((fail+1)); printf '  FAIL  %s: did not want [%s]\n' "$1" "$2" ;;
           *) pass=$((pass+1)); printf '  ok    %s\n' "$1" ;; esac; }

echo "the roster — who the sentinel may summon"

# A FIXTURE CHAMBER, NOT THE REAL ONE. The shipped $SPIRA_FAYTHS omits the concierge on every
# host today, so a suite reading the live roster would pass for a reason that has nothing to do
# with FAYTH_SUMMON — and would keep passing after the property was deleted.
CH="$TMP/chamber"; mkdir -p "$CH"
cat > "$CH/worker.fayth" <<'EOF'
FAYTH_NAME=worker
FAYTH_LABELS="spira,plan"
EOF
cat > "$CH/laner.fayth" <<'EOF'
FAYTH_NAME=laner
FAYTH_LABELS="spira,incident"
FAYTH_LANE=ops
EOF
cat > "$CH/human.fayth" <<'EOF'
FAYTH_NAME=human
FAYTH_SUMMON=operator
EOF
# A persona that is BOTH operator-summoned and in a lane. FAYTH_SUMMON must win: the lane
# loop is a second door into the sentinel, and a fayth kept out of one list and handed to the
# other is summoned exactly as if nothing had been declared.
cat > "$CH/humanlane.fayth" <<'EOF'
FAYTH_NAME=humanlane
FAYTH_LABELS="spira,incident"
FAYTH_LANE=ops
FAYTH_SUMMON=operator
EOF

roster() { # roster <function>
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$TMP" SPIRA_FAYTHS="worker laner human humanlane" \
        bash -c '. "$2"/lib.sh 2>/dev/null; "$1"' _ "$1" "$HERE" 2>/dev/null
}
# The fixture chamber must sit under SPIRA_HOME, which is where fayth_get and fayth_names look.
ln -sfn "$CH" "$TMP/chamber"

is "the task pool is the ordinary fayth alone"        "worker"          "$(roster spira_task_fayths)"
is "the lane list is the ordinary lane fayth alone"   "laner"           "$(roster spira_lane_fayths)"
# fayth_names sorts, so both operator personas appear in this order.
is "and the operator personas are named as such"      "human humanlane" "$(roster spira_operator_fayths)"

nowant "an operator persona is never in the task pool"  "human" "$(roster spira_task_fayths)"
nowant "nor in the lane list, which is the second door" "human" "$(roster spira_lane_fayths)"

# THE POSITIVE CONTROL FOR THE CONTROL. Strip FAYTH_SUMMON from the fixture and the same
# persona MUST appear — otherwise these assertions would pass against a roster that was empty
# for some unrelated reason, which is the shape of a test that guards nothing.
printf 'FAYTH_NAME=human\n' > "$CH/human.fayth"
want "without FAYTH_SUMMON that persona IS summoned" "human" "$(roster spira_task_fayths)"

echo
echo "the shipped concierge — real persona, summoned by nobody"

ship() { # ship <function>   — the REAL chamber, with the concierge listed in the roster
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$HERE" SPIRA_FAYTHS="builder ops concierge" \
        bash -c '. "$2"/lib.sh 2>/dev/null; "$1"' _ "$1" "$HERE" 2>/dev/null
}
# LISTED IN $SPIRA_FAYTHS ON PURPOSE. No host lists it today, so the roster alone would keep
# it out — and that is a second reason, not the one under test. Naming it here removes the
# reason that is doing the work by accident and leaves only FAYTH_SUMMON holding the line.
nowant "the shipped concierge is not in the task pool"   "concierge" "$(ship spira_task_fayths)"
nowant "the shipped concierge is not in the lane list"   "concierge" "$(ship spira_lane_fayths)"
want   "the shipped concierge IS an operator persona"    "concierge" "$(ship spira_operator_fayths)"
want   "and the other personas are still summonable"     "builder"   "$(ship spira_task_fayths)"

echo
echo "the brief — composed, or refused"

# Brief rendering requires statutes in the rule database. A fresh container has none,
# so these tests skip rather than fail when rule.sh list returns nothing. The assertions
# run in full on an operator host where the statute book is populated.
if ! bash "$HARNESS/rule.sh" list 2>/dev/null | grep -q .; then
    printf '  skip  (brief: statute book is empty in this environment — skipping brief tests)\n'
else

BRIEF="$(bash "$HARNESS/concierge.sh" brief 2>"$TMP/err")"
if [ -n "$BRIEF" ] && [ -f "$BRIEF" ]; then
    pass=$((pass+1)); printf '  ok    concierge.sh brief renders a file\n'
    B="$(cat "$BRIEF")"
    # EVERY PLACEHOLDER, because an unsubstituted one is a command line the session will try
    # to run. The failure arrives hours later as "the concierge does not escalate anything".
    nowant "no placeholder survives rendering"  "{{"          "$B"
    want   "the brief names the ask path"       "ask.sh add"  "$B"
    want   "and the bead contract"              "bead.sh file" "$B"
    want   "and carries the statute book"       "# Memories in force" "$B"
    # THE STATUTES THIS ROLE IS ACTUALLY HELD TO, IN FULL TEXT — not as index slugs. This is
    # the whole reason FAYTH_STATUTE_CORE exists: the shipped core set is builder-shaped, and
    # the statute the operator's own session violated for 39 turns was outside it.
    for law in law-the-harness-checkout-is-production law-filed-bead-queued-xor-escalated \
               law-decisions-surface-immediately law-closed-is-not-landed; do
        want "  $law is rendered in full" "## $law" "$B"
    done
else
    fail=$((fail+1)); printf '  FAIL  concierge.sh brief produced nothing:\n%s\n' "$(cat "$TMP/err")"
fi

# A MISSING BRIEF IS A REFUSAL, NOT A DEGRADED START — the assertion that the refusal exists.
# Pointed at a persona with no markdown, it must fail loudly rather than launch a session whose
# only difference from a working one is that it was never told anything.
out="$(CONCIERGE_FAYTH=no-such-persona bash "$HARNESS/concierge.sh" brief 2>&1)"; rc=$?
is   "a missing brief exits non-zero"     1 "$rc"
want "and says which file was missing"    "no-such-persona.md" "$out"

# A CORE SET THAT RENDERS NOTHING IN FULL IS A TYPO, AND IT IS THE SILENT ONE. render_memories
# matches core slugs EXACTLY and demotes anything it does not recognise to the index tier
# without a word, so a mistyped or retired slug costs that statute its full text and says
# nothing at all. The brief still looks complete — right size, every placeholder filled, the
# law apparently present — which is why this needs an assertion rather than a reader.
#
# A FIXTURE PERSONA, NOT THE SHIPPED ONE. Editing chamber/concierge.fayth to drive this would
# leave the suite one failed assertion away from having corrupted the thing it tests.
FX="$TMP/fx"; mkdir -p "$FX/chamber"
cp "$HERE/chamber/concierge.md" "$FX/chamber/typo.md"
sed -e 's|^FAYTH_STATUTE_CORE=.*|FAYTH_STATUTE_CORE="law-slug-that-does-not-exist"|' \
    -e 's|^FAYTH_NAME=.*|FAYTH_NAME=typo|' \
    "$HERE/chamber/concierge.fayth" > "$FX/chamber/typo.fayth"
# The launcher reads $SPIRA_HOME/chamber, so the fixture chamber has to be the one it finds.
# Everything else about the harness stays real: the point is that the STATUTE lookup misses.
out="$(SPIRA_HOME="$FX" CONCIERGE_FAYTH=typo bash "$HARNESS/concierge.sh" brief 2>&1)"; rc=$?
is   "an all-typo core set exits non-zero"  1 "$rc"
want "and says the slugs were demoted"      "no statute rendered in full" "$out"

# THE POSITIVE CONTROL FOR THAT REFUSAL. The same fixture with ONE real slug must compose —
# otherwise the assertion above would pass against a fixture that was broken for some
# unrelated reason, which is most of them (law-absence-needs-a-positive-control).
sed -i 's|^FAYTH_STATUTE_CORE=.*|FAYTH_STATUTE_CORE="law-closed-is-not-landed"|' "$FX/chamber/typo.fayth"
out="$(SPIRA_HOME="$FX" CONCIERGE_FAYTH=typo bash "$HARNESS/concierge.sh" brief 2>&1)"; rc=$?
is   "one real slug composes a brief"       0 "$rc"
want "and renders that statute in full"     "## law-closed-is-not-landed" "$(cat "$out" 2>/dev/null)"

# THE SUMMARY LINE IS READ BY THE OPERATOR AND BY NOBODY ELSE, which is exactly why it needs
# an assertion: `here` grew its own inline count, the quoting came out wrong, and it reported
# "0 statutes in full" about a brief holding twenty. A launcher reporting on itself is the
# reading nobody goes behind.
#
# AND IT IS PARSED, NOT SUBSTRING-MATCHED. The first version of this check asserted that the
# line does not CONTAIN "0 statutes" and failed against the correct output, because "20
# statutes" contains it. A count is a number; compare it as one.
sum="$(bash "$HARNESS/concierge.sh" here --help 2>&1 >/dev/null | head -1)"
want "the summary names a count and a size" "statutes in full," "$sum"
n_sum="$(sed -n 's/^concierge: \([0-9]*\) statutes in full.*/\1/p' <<<"$sum")"
if [ -n "$n_sum" ] && [ "$n_sum" -gt 0 ] 2>/dev/null; then
    pass=$((pass+1)); printf '  ok    and the count is %s, not zero\n' "$n_sum"
else
    fail=$((fail+1)); printf '  FAIL  the summary reported [%s] statutes: %s\n' "$n_sum" "$sum"
fi
# THE COUNT MUST BE THE BRIEF'S OWN, not a constant that happens to look plausible.
is "and it matches the rendered brief" \
   "$(grep -c '^## law-' "$(bash "$HARNESS/concierge.sh" brief)" 2>/dev/null)" "$n_sum"


echo
echo "the brief — no-wiki install (SPIRA_WIKI unset)"

# SPIRA_WIKI UNSET: every path the brief names must be a file that exists on this host.
# The filing tool ships in the harness (spira/bead.sh) and is referenced via {{BEAD}}, so
# its rendered path must exist regardless of whether a wiki is configured.
# THE POSITIVE CONTROL: the brief must still name the bead tool; absence of .claude/bead.sh
# alone would pass just as well against a brief that named nothing at all.
BRIEF_NW="$(SPIRA_WIKI= bash "$HARNESS/concierge.sh" brief 2>"$TMP/err_nw")"
if [ -n "$BRIEF_NW" ] && [ -f "$BRIEF_NW" ]; then
    pass=$((pass+1)); printf '  ok    no-wiki brief renders\n'
    BNW="$(cat "$BRIEF_NW")"
    nowant "no-wiki brief does not name the wiki-relative tool"  ".claude/bead.sh" "$BNW"
    want   "no-wiki brief still names the harness bead tool"     "bead.sh file"    "$BNW"
    bead_path="$(printf '%s\n' "$BNW" | grep -oE '[^ ]+bead\.sh' | head -1)"
    if [ -n "$bead_path" ] && [ -f "$bead_path" ]; then
        pass=$((pass+1)); printf '  ok    bead tool path in brief exists: %s\n' "$bead_path"
    else
        fail=$((fail+1)); printf '  FAIL  bead tool path in brief does not exist: [%s]\n' "${bead_path:-<not found>}"
    fi
else
    fail=$((fail+1)); printf '  FAIL  no-wiki brief failed:\n%s\n' "$(cat "$TMP/err_nw")"
fi

fi  # statute book guard

echo
echo "concierge self-test: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
