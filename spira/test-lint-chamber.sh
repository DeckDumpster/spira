#!/usr/bin/env bash
#
# test-lint-chamber.sh — chamber integrity: the T0 lints that need no database and no git
# fixture at all.
#
# Extracted from test-spike.sh (UC-safety-fences-33), which ran these as pure text reads
# inside a T3 suite that also builds a Dolt fixture and a bare git remote — every one of
# these rows paid for that setup without needing any of it. Nothing here changed: same
# checks, same positive controls, run on their own.
#
# tier: T0
# covers: spira/chamber/*.md spira/chamber/*.fayth spira/aeon.sh spira/lib.sh UC-safety-fences-33
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-lint-chamber.sh"

# ======================================================================================
echo
echo "the persona is installed, and every placeholder in its brief is one its filler fills:"
# ======================================================================================
# STRUCTURAL, AND CHEAP, AND FIRST — it needs no database, so it still runs on a box where
# the fixture server is down. A `{{PLACEHOLDER}}` nothing substitutes is not an error
# anywhere: the brief simply reaches the agent with the literal braces in it, telling it to
# write to a directory named `{{SPIKE_DIR}}`. Nothing else would notice.
#
# EACH BRIEF IS CHECKED AGAINST THE PROGRAM THAT ACTUALLY RENDERS IT, not against aeon.sh.
# The chamber holds briefs for agents that are not aeons and are filled by their own script,
# and a check that assumed one filler failed the moment a second kind of brief was added —
# reporting ten missing substitutions in a brief that was entirely correct.
#
# THE DISCRIMINATOR IS WHO SUMMONS THE PERSONA, not whether a `.fayth` sits beside the brief.
# It was the latter, and it broke on the concierge: an operator persona has a full fayth —
# model, memories, statute core — and is nevertheless filled by its own launcher, because
# nothing summons it. Under the old rule its brief was checked against aeon.sh's substitution
# list and reported five missing placeholders that concierge.sh fills correctly.
#
# So: a fayth the sentinel can summon is aeon.sh's; anything else is filled by the script of
# the same name, which may sit beside this suite or at the harness root.
unfilled() {                    # unfilled <brief> <filler> -> the placeholders it leaves behind
    local f="$1" filler="$2" ph key missing=""
    for ph in $(grep -o '{{[A-Z_]*}}' "$f" | sort -u); do
        key="${ph#\{\{}"; key="${key%\}\}}"
        # TWO FORMS, because a filler substitutes in two ways: a sed script for the simple
        # values, and bash parameter expansion for the multi-line ones — whose replacement is
        # a whole `bd show` and would take a sed script apart on the first slash in it.
        # `grep -F`, because the second form is written with backslash-escaped braces and
        # every regex dialect reads those as something else.
        grep -qF "{{$key}}" "$filler" \
          || grep -qF "\\{\\{$key\\}\\}" "$filler" \
          || missing="$missing $ph"
    done
    printf '%s' "$missing"
}

# THE POSITIVE CONTROL COMES FIRST. Every assertion below is that a matcher found nothing,
# and a matcher pointed at the wrong file finds nothing too — so it is made to name a planted
# offender before its silence is worth anything (law-absence-needs-a-positive-control).
PLANT="$(mktemp -d)"
printf 'write to {{NOWHERE}}, and also {{DB}}\n' > "$PLANT/planted.md"
want   "the placeholder check can see an unfilled one" "{{NOWHERE}}" \
       "$(unfilled "$PLANT/planted.md" "$HERE/aeon.sh")"
nowant "and does not accuse one that is filled"        "{{DB}}" \
       "$(unfilled "$PLANT/planted.md" "$HERE/aeon.sh")"

# {{DEADLINE}} IS NAMED HERE rather than left to the loop below, because it is the one
# placeholder that fails silently in both directions. Unfilled, the Ops aeon is told it dies
# at the literal `{{DEADLINE}}`; dropped from the brief altogether, it is told nothing at all
# and behaves exactly as it did before there was a deadline to see — four consecutive
# sessions on one incident, each killed at the wall, no commit and no bead between them. The
# loop below catches the first case for every brief; the pair here fixes it to this name and
# the assertion further down requires the shipped Ops brief to carry it.
: > "$PLANT/nofiller.sh"
printf 'this session is killed at {{DEADLINE}}\n' > "$PLANT/deadline.md"
want "an unfilled {{DEADLINE}} fails this suite" "{{DEADLINE}}" \
     "$(unfilled "$PLANT/deadline.md" "$PLANT/nofiller.sh")"
is   "and aeon.sh is a filler that fills it"     "" \
     "$(unfilled "$PLANT/deadline.md" "$HERE/aeon.sh")"
rm -rf "$PLANT"

for f in "$HERE"/chamber/*.md; do
    n="$(basename "$f" .md)"
    summon=auto
    [ -f "$HERE/chamber/$n.fayth" ] && summon="$(sed -n 's/^FAYTH_SUMMON=//p' "$HERE/chamber/$n.fayth" | tr -d '"' | tail -1)"
    [ -n "$summon" ] || summon=auto
    if [ -f "$HERE/chamber/$n.fayth" ] && [ "$summon" = auto ]; then
        filler="$HERE/aeon.sh"
    elif [ -f "$HERE/$n.sh" ]; then
        filler="$HERE/$n.sh"
    else
        filler="$HERE/../$n.sh"
    fi
    if [ ! -f "$filler" ]; then
        bad "every placeholder in $n.md is substituted" "no filler: $(basename "$filler") does not exist"
        continue
    fi
    is "every placeholder in $n.md is substituted by $(basename "$filler")" "" "$(unfilled "$f" "$filler")"
done

# EVERY COMMAND A BRIEF NAMES MUST EXIST. The placeholder check above proves the TEMPLATE
# mechanism works; it says nothing about what the filled-in text points AT. On 2026-09-08 all
# four briefs passed it while naming eight commands under `.claude/` — a path that had not
# existed in either repository since the harness split out of brain (sp-9tal). Every Ops
# session was told to match an SOP, write an SOP, file a bead and escalate using programs
# that were not there, so step 1 of its loop failed before it began and the closing rule
# could not be obeyed at all. A green suite reported none of it.
#
# So: render each brief the way its filler does, then check that the first word of every
# fenced/indented command line that looks like a path resolves to a real executable.
# law-absence-needs-a-positive-control — the control is the deliberately broken path below.
#
# SPIRA_COCKPIT MAY NOT BE SET in a minimal test environment — conf.sh derives it from
# SPIRA_HOME, but suites run without sourcing conf.sh. Derive the same default from HERE
# so the sed substitution below has a real path to fill {{ASK}} with, and the command-exist
# check means something rather than silently checking a path that starts with "/ask.sh".
: "${SPIRA_COCKPIT:=$(dirname "$HERE")/cockpit}"
for f in "$HERE"/chamber/*.md; do
    n="$(basename "$f" .md)"
    missing=""
    while read -r cand; do
        [ -n "$cand" ] || continue
        [ -x "$cand" ] || missing="$missing $cand"
    done < <(
        sed -e "s|{{SOP}}|$HERE/sop.sh|g" -e "s|{{INCIDENT}}|$HERE/incident.sh|g" \
            -e "s|{{ASK}}|$HERE/mail.sh|g" \
            -e "s|{{SUITES}}|$HERE/suites.sh|g" "$f" |
        grep -oE '(^|[`( ])/[A-Za-z0-9_./-]+\.sh' | tr -d '`( ' | sort -u
    )
    is "every command $n.md names exists and is executable" "" "$missing"
done

# The control: a brief naming a path that is not there must FAIL the check above.
probe="$(mktemp)"; printf 'run it:\n\n    /nonexistent/definitely-not-here.sh list\n' > "$probe"
probe_missing=""
while read -r cand; do [ -n "$cand" ] && [ ! -x "$cand" ] && probe_missing="$probe_missing $cand"; done < <(
    grep -oE '(^|[`( ])/[A-Za-z0-9_./-]+\.sh' "$probe" | tr -d '`( ' | sort -u)
[ -n "$probe_missing" ] && ok "the command check can see a path that does not exist" \
    || bad "the command check can see a path that does not exist" "it saw nothing"
rm -f "$probe"

[ -f "$HERE/chamber/spike.fayth" ] && ok "spike.fayth is in the chamber" \
    || bad "spike.fayth is in the chamber" "absent"
[ -f "$HERE/chamber/spike.md" ] && ok "spike.md is in the chamber" \
    || bad "spike.md is in the chamber" "absent"

# THE BRIEF IS THE MECHANISM for everything the tool list no longer enforces, so its load-
# bearing clauses are asserted rather than trusted. Each of these is a rule that has no other
# home: drop the sentence and nothing anywhere fails.
brief="$(cat "$HERE/chamber/spike.md")"
want "the brief demands two or more costed options" "each with a cost and a risk" "$brief"
want "and a recommendation rather than a survey"    "Commit to one option"        "$brief"
want "and a named falsifier"                        "falsifier"                   "$brief"
want "and says a recommendation AGAINST is a complete answer" \
     "\"No\" is a complete answer"                                                "$brief"
want "and that sources are kept verbatim"           "preserved verbatim"          "$brief"
want "and that a POC goes on a branch of its own"   "branch of its own"           "$brief"
want "and that it must not leave a merge"           "must not leave a merge"      "$brief"
want "and that its context is the bead, not a conversation" "ids rather than bodies" "$brief"

# THE OPS BRIEF'S WALL, asserted here because Ops is the only persona killed on a clock and
# the brief is the whole of the mechanism: drop these clauses and nothing anywhere fails,
# while every Ops session goes back to spending its last minute on an investigation it will
# not get to finish. The deadline itself is rendered by aeon.sh — that it reaches the model
# as a real time rather than as braces is asserted in test-aeon-verdict.sh, against the
# actual render.
ops_brief="$(cat "$HERE/chamber/ops.md")"
want "the ops brief tells the aeon when its session is killed" "{{DEADLINE}}" "$ops_brief"
want "and makes the wrap-up a hard rule with a number in it" "At 90 seconds left, stop" "$ops_brief"
want "and says the rule outranks the loop"     "outranks every step below it" "$ops_brief"
want "and names where a finding goes instead"  "{{INCIDENT}} file"            "$ops_brief"
# A LITERAL WALL IN THE PROSE IS A SECOND SOURCE OF TRUTH for a number that lives in
# ops.fayth, and the brief is the copy nobody edits when the key changes.
nowant "and does not restate the wall as a literal" "eight minutes" "$ops_brief"

# ONLY OPS. A builder or a spike runs until its work is done — a clock cannot tell slow from
# stuck, and killing on one charged an attempt toward poison for being legitimately long. So
# a deadline in either of those briefs would render as "no wall-clock deadline" and a wrap-up
# rule in them would be an instruction to hurry against nothing.
for n in builder spike; do
    nowant "the $n brief carries no deadline"      "{{DEADLINE}}" "$(cat "$HERE/chamber/$n.md")"
    nowant "and the $n fayth declares no wall"     "FAYTH_TIMEOUT_SECONDS" \
           "$(grep -v '^[[:space:]]*#' "$HERE/chamber/$n.fayth")"
done
want "while the ops fayth is the one that declares it" "FAYTH_TIMEOUT_SECONDS" \
     "$(grep -v '^[[:space:]]*#' "$HERE/chamber/ops.fayth")"

# The fayth's own fields (predicate built from the configured label, tool list) are a
# test-fayth.sh row.

# ======================================================================================
echo
echo "a refusal hands the bead back through the one helper that clears the claim:"
# ======================================================================================
# A REFUSED SPIKE IS A REOPEN LIKE ANY OTHER, and this is where the rule is enforced for all
# of them. `bd reopen` leaves the assignee in place; `bd ready --claim` skips an assigned bead
# while `bd ready` still lists it. So a reopen that forgets to release the claim puts the bead
# back on the board wearing a dead aeon's name — visible, counted as ready, and claimable by
# nobody. That failure is invisible by construction: `bd reopen` exits 0, the bead really does
# go back to open, and only the claim that never comes says otherwise.
#
# So the rule is structural rather than a comment: `reopen` is called in lib.sh's bead_reopen
# and nowhere else. It is checked here because it is one more static fact about the chamber's
# scripts, and because the check has already caught one site whose correctness rested on a
# clearing sixty lines away that no later edit to either could see.
reopen_sites() {                # every direct `bd … reopen` outside the helper that owns it
    # COMMENTS ARE NOT CALL SITES. The scar is explained in prose in several of these files,
    # so a matcher that reads `# \`bd reopen\` keeps the assignee` as a violation fails on the
    # very comments saying why the rule exists — and a check that goes red for documenting
    # itself gets deleted rather than obeyed.
    #
    # AND NEITHER ARE QUOTED STRINGS, which is the same mistake one layer in. aeon.sh writes
    # a NOTE whose text reads "The landing pass will reopen this bead." — prose, inside an
    # argument to `bdq note`, reopening nothing. The comment filter above did not see it
    # because it is not a comment, so this check went red on every push to main from
    # 2026-09-23 and took the release with it: gate red means no cut, no publish, no
    # acceptance. A lint that fails on a sentence ABOUT the rule is the exact failure the
    # paragraph above was written to prevent; it just needed to cover one more case.
    #
    # The awk strips double-quoted spans and re-tests, but PRINTS THE ORIGINAL LINE, so a
    # real violation still reports readable source rather than a mangled remnant.
    grep -rnE '\b(bd|bdq)[A-Za-z_]* +[^|;&#]*\breopen\b' "$1"/*.sh 2>/dev/null \
      | grep -vE '^[^:]*:[0-9]+: *#' \
      | grep -v '/lib\.sh:' | grep -v '/test-' \
      | awk '{ s = $0; gsub(/"[^"]*"/, "", s);
               if (s ~ /(bd|bdq)[A-Za-z_]*[ \t]+[^|;&#]*reopen/) print $0 }'
}
PLANT="$(mktemp -d)"; cp "$HERE"/*.sh "$PLANT/" 2>/dev/null
printf 'bdq reopen "$id"\n' > "$PLANT/planted.sh"
want "the reopen check can see a direct call at all" "planted.sh" "$(reopen_sites "$PLANT")"
rm -rf "$PLANT"

# NEGATIVE CONTROL, and it is the whole point of the awk above. Without it this plant is
# reported as a violation and every push to main goes red on a sentence.
PLANT2="$(mktemp -d)"
printf 'bdq note "$id" "The landing pass will reopen this bead." >/dev/null\n' > "$PLANT2/prose.sh"
nowant "reopen inside a quoted argument is not a call site" "prose.sh" "$(reopen_sites "$PLANT2")"
# And the positive control must still fire in the same directory, so the negative control
# cannot be passing merely because the matcher stopped matching anything at all.
printf 'bdq reopen "$id"\n' > "$PLANT2/real.sh"
want "a real call beside it is still caught" "real.sh" "$(reopen_sites "$PLANT2")"
rm -rf "$PLANT2"
is   "and no harness script reopens a bead outside bead_reopen" "" "$(reopen_sites "$HERE")"

tl_summary
