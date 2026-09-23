#!/usr/bin/env bash
#
# test-spike.sh — the spike persona: its partition is its own, and its branch may carry a
# document and nothing else.
#
#   ./test-spike.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# The spike persona is defined almost entirely by two things a shell cannot check for itself:
# a predicate that must select its own beads and nobody else's, and a rule about what its
# branch is allowed to leave behind. The second is the one with teeth. A spike is given the
# full toolset on purpose — for most interesting questions the only honest answer to "is this
# feasible" comes from trying it — so nothing stops it committing an experiment beside its
# write-up, and the landing worker would then merge the experiment into the base while every
# downstream check passed: the aeon committed, the commit names the bead, the gate ran, the
# branch landed. `confine.sh` is the refusal, and this is the suite that holds it.
#
# THE POSITIVE CONTROL COMES FIRST and everything after it is read through that. A confinement
# check that finds nothing and one pointed at the wrong bead look identical from the outside,
# and the wrong one reads as all-clear (law-absence-needs-a-positive-control). So an offender
# is planted and the matcher is required to name it before any silence here is believed.
#
# A REAL `bd` on a fixture database, and REAL git with a real bare remote. Every claim about
# confinement is a claim about what `git diff A...B` reports, and every claim about the
# partition is read through `bd ready` — a model of either is a second implementation, and the
# two disagreeing is a bug in neither and a failure in both.
#
# SPIRA_SPIKE_PATHS AND SPIRA_SPIKE_LABEL ARE PINNED TO NON-DEFAULTS throughout. Asserting
# against the shipped defaults would pass just as well if the code had the literal written in,
# which is the thing those keys exist to prevent.
#
# THE CONFINEMENT FENCE IS CLAIMED HERE ALONGSIDE THE WORKER THAT CALLS IT. `confine.sh` alone
# is not the property under test — the failure it exists to stop is a landing pass that never
# consults it, which is why a change to `landing.sh` has to select this suite too.
#
# defect: sp-qkf
# covers: spira/confine.sh spira/landing.sh spira/chamber/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-spike.sh"

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

# The fayth is a shell fragment that gets SOURCED into the summoning process. A syntax error
# in it is not a persona that misbehaves, it is a harness that dies mid-summon.
for f in "$HERE"/chamber/*.fayth; do
    bash -n "$f" 2>/dev/null && ok "$(basename "$f") parses" || bad "$(basename "$f") parses" "syntax error"
done
bash -n "$HERE/confine.sh" && ok "confine.sh parses" || bad "confine.sh parses" "syntax error"

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

# The fayth's own fields. The predicate is built from the configured label rather than a
# literal, which is the property that keeps the fayth, the brief and the fence agreeing.
fayth_src="$(cat "$HERE/chamber/spike.fayth")"
want "the predicate is built from the configured label" 'FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}$SPIRA_SPIKE_LABEL"' "$fayth_src"
nowant "and does not hardcode one"                      'FAYTH_LABELS="spira,spike"'               "$fayth_src"
want "a spike may search the web"                       "WebSearch"                                "$fayth_src"
want "and it may build"                                 "Bash"                                     "$fayth_src"
want "and edit"                                         "Edit"                                     "$fayth_src"

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
# and nowhere else. It is checked here because the confinement refusal is a reopen path, and
# because the check has already caught one site whose correctness rested on a clearing sixty
# lines away that no later edit to either could see.
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

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-spike

TMP="$(mktemp -d)"
KIDS=()
cleanup() { for p in "${KIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done
            testdb_drop; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM
testdb_up spike || { echo "test-spike: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# NON-DEFAULTS, all three. `notes` is not `docs/spikes` and `research` is not `spike`, so a
# literal written into confine.sh or into lib.sh fails here rather than passing by luck.
export SPIRA_SPIKE_LABEL=research
export SPIRA_SPIKE_DIR=notes/spikes
export SPIRA_SPIKE_PATHS="notes/spikes sources"
export SPIRA_ASK_LABEL=needs-a-human

# ======================================================================================
echo
echo "the partition is the spike's own:"
# ======================================================================================
export SPIRA_RUN="$TMP/run"
export SPIRA_HOME="$TMP/home"
mkdir -p "$SPIRA_RUN" "$SPIRA_HOME/chamber"
printf '#!/bin/sh\nexit 0\n' > "$SPIRA_HOME/aeon.sh"; chmod +x "$SPIRA_HOME/aeon.sh"
SUMMONED="$TMP/summoned.txt"
export SPIRA_SUMMON="$TMP/summon.sh"
printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> %s\n' "$SUMMONED" > "$SPIRA_SUMMON"
chmod +x "$SPIRA_SUMMON"

# THE REAL FAYTH FILES, copied rather than reconstructed. A fixture that writes its own
# predicate is asserting about the fixture; what has to be true is that the persona SHIPPED
# in the chamber selects its own beads.
cp "$HERE/chamber/builder.fayth" "$HERE/chamber/spike.fayth" "$SPIRA_HOME/chamber/"

beads() { testdb_reset; [ $# -gt 0 ] || return 0; printf '%s\n' "$@" | testdb_seed; }
bead() {  # bead <id> <labels-csv> [type] [status]
    printf '{"id":"%s","title":"t %s","status":"%s","issue_type":"%s","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
      "$1" "$1" "${4:-open}" "${3:-task}" "$(printf '"%s",' ${2//,/ } | sed 's/,$//')"
}
beads
# shellcheck disable=SC1090
. "$HERE/lib.sh"
summon() { : > "$SUMMONED"; summon_fayth "$1" >"$TMP/log" 2>&1; printf '%s' "$?"; }

want "the shipped fayth is discovered without being listed" "spike" "$(spira_fayths)"

beads "$(bead sp-spike-1 "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL")"
is "a spike bead is in the spike's partition" 1 "$(fayth_ready spike)"
is "and not in the builder's"                 0 "$(fayth_ready builder)"
is "so the spike is summoned"                 0 "$(summon spike)"
is "and the builder is not"                   1 "$(summon builder)"

beads "$(bead sp-plan-1 "${SPIRA_SCOPE_LABEL},plan")"
is "a plan bead is not in the spike's partition" 0 "$(fayth_ready spike)"
is "and the spike is not summoned for it"        1 "$(summon spike)"
is "while the builder is"                        0 "$(summon builder)"

# AND over the labels, never OR. A bead carrying the spike label without `spira` belongs to
# something else — an installation that imported a predecessor's beads holds thousands of
# them, and a persona that ORs its way in races a live worker.
beads "$(bead xx-1 "$SPIRA_SPIKE_LABEL")"
is "a partial label match is not in the partition" 0 "$(fayth_ready spike)"
beads "$(bead sp-spike-2 "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL,$SPIRA_ASK_LABEL")"
is "an escalation is never dispatched as spike work" 0 "$(fayth_ready spike)"
beads "$(bead sp-spike-3 "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL,spira-poison")"
is "nor is a poisoned spike"                         0 "$(fayth_ready spike)"

# ======================================================================================
echo
echo "confinement: the positive control first"
# ======================================================================================
REPO="$TMP/repo"
git init -q -b main "$REPO"
mkdir -p "$REPO/notes/spikes" "$REPO/src"
echo base > "$REPO/src/lib.rs"
git -C "$REPO" add -A && git -C "$REPO" commit -q -m base

# spike_branch <id> <file>... — a branch off main carrying exactly these files
spike_branch() {
    local id="$1"; shift
    git -C "$REPO" checkout -q -B "spira/$id" main
    local f
    for f in "$@"; do mkdir -p "$REPO/$(dirname "$f")"; echo "$id" > "$REPO/$f"; done
    git -C "$REPO" add -A && git -C "$REPO" commit -q -m "$id — work"
    git -C "$REPO" checkout -q main
}
confine() { bash "$HERE/confine.sh" "$1" "spira/$1" "$REPO" main 2>&1; }
confine_rc() { confine "$1" >/dev/null 2>&1; printf '%s' "$?"; }

beads "$(bead sp-poc "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL" task closed)"
spike_branch sp-poc notes/spikes/question.md src/poc.rs
is   "a spike branch carrying code is REFUSED" 1 "$(confine_rc sp-poc)"
want "and the refusal names the offending path" "src/poc.rs" "$(confine sp-poc)"
want "and says what a spike may land"           "notes/spikes" "$(confine sp-poc)"
want "and names the remedy rather than the rule alone" "branch of their own" "$(confine sp-poc)"
nowant "and does not accuse the document"       "notes/spikes/question.md
" "$(confine sp-poc | sed -n '/^outside:/,$p')"

# Only now is a silence worth anything.
beads "$(bead sp-doc "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL" task closed)"
spike_branch sp-doc notes/spikes/question.md
is "a spike branch carrying only its document is allowed" 0 "$(confine_rc sp-doc)"
beads "$(bead sp-src "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL" task closed)"
spike_branch sp-src notes/spikes/q.md sources/fetched.md
is "and the second configured tree is allowed too" 0 "$(confine_rc sp-src)"

# ======================================================================================
echo
echo "the fence binds the persona, not the path:"
# ======================================================================================
# A guard that bound the PATH would bind whoever is most disciplined about using it and miss
# the actor it was aimed at. The builder's whole job is to change `src/`.
beads "$(bead sp-build "${SPIRA_SCOPE_LABEL},plan" task closed)"
spike_branch sp-build src/feature.rs
is "a plan bead's branch is not confined" 0 "$(confine_rc sp-build)"
is "and neither is its output examined"   "" "$(confine sp-build)"

# ...and a bead nobody can read fails OPEN. This check stands between finished work and its
# base: a `bd` that times out must not become a harness that silently stops landing anything.
beads
spike_branch sp-gone src/anything.rs
is "an unreadable bead is not treated as a spike" 0 "$(confine_rc sp-gone)"

# ======================================================================================
echo
echo "the allowed trees are path prefixes, not string prefixes:"
# ======================================================================================
# `notes/spikes-scratch` is exactly the name an aeon reaches for when told to keep its
# experiment beside its notes, and a naive `case $f in $p*)` admits it.
beads "$(bead sp-adj "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL" task closed)"
spike_branch sp-adj notes/spikes-scratch/poc.rs
is   "an adjacent directory is outside" 1 "$(confine_rc sp-adj)"
want "and is named as such" "notes/spikes-scratch/poc.rs" "$(confine sp-adj)"

# A deep path inside an allowed tree is inside it.
beads "$(bead sp-deep "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL" task closed)"
spike_branch sp-deep notes/spikes/sources/2026/a.jsonl
is "a deep path inside an allowed tree is allowed" 0 "$(confine_rc sp-deep)"

# ======================================================================================
echo
echo "the diff is against the merge base, not against the tip:"
# ======================================================================================
# `diff A..B` is every difference between two tips, so a base that moved ahead reports files
# the branch never touched as the branch's offence. `diff A...B` is the branch's own work.
beads "$(bead sp-behind "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL" task closed)"
spike_branch sp-behind notes/spikes/behind.md
echo moved > "$REPO/src/unrelated.rs"
git -C "$REPO" add -A && git -C "$REPO" commit -q -m "main moves on"
is   "a branch behind its base is judged on its own work" 0 "$(confine_rc sp-behind)"
nowant "and is not accused of the base's changes" "src/unrelated.rs" "$(confine sp-behind)"

# ======================================================================================
echo
echo "and the landing worker actually asks:"
# ======================================================================================
# The wiring, end to end. confine.sh passing in isolation proves nothing about a landing
# worker that never calls it — which is the failure mode a persona-level suite is built to
# catch, and the one ops.fayth sat in for a day.
REMOTE="$TMP/remote.git"; RUN="$TMP/lrun"; SH="$TMP/lspira"
LREPO="$TMP/lrepo"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$LREPO"
git -C "$LREPO" commit -q --allow-empty -m base
git -C "$LREPO" remote add origin "$REMOTE"
git -C "$LREPO" push -q origin main
git -C "$LREPO" fetch -q origin
mkdir -p "$RUN/worktree" "$SH"
cp "$HERE/landing.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/confine.sh" "$SH/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$SH/gate.sh"; chmod +x "$SH/gate.sh"
printf 'home | %s | push | origin/main | |\n' "$LREPO" > "$SH/repo-map"

land() {
    rm -f "$RUN/landing.progress"
    SPIRA_HOME="$SH" SPIRA_RUN="$RUN" SPIRA_DB="$SPIRA_DB" SPIRA_REPO="$LREPO" \
    SPIRA_HOME_REPO=home SPIRA_REPO_MAP="$SH/repo-map" \
    SPIRA_SPIKE_LABEL="$SPIRA_SPIKE_LABEL" SPIRA_SPIKE_DIR="$SPIRA_SPIKE_DIR" \
    SPIRA_SPIKE_PATHS="$SPIRA_SPIKE_PATHS" \
        bash "$SH/landing.sh" 2>&1
}
status_of() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("status") or "")'; }
assignee_of() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import json, sys
d = json.load(sys.stdin); d = d if isinstance(d, list) else [d]
print(d[0].get("assignee") or "")'; }
land_branch() {   # land_branch <id> <file>...
    local id="$1"; shift
    git -C "$LREPO" worktree add -q -b "spira/$id" "$RUN/worktree/$id" main
    local f
    for f in "$@"; do mkdir -p "$RUN/worktree/$id/$(dirname "$f")"; echo "$id" > "$RUN/worktree/$id/$f"; done
    git -C "$RUN/worktree/$id" add -A
    git -C "$RUN/worktree/$id" commit -q -m "$id — work"
}

beads "$(bead sp-land-doc "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL,repo:home" task closed)"
land_branch sp-land-doc notes/spikes/answer.md
out="$(land)"
want "a confined spike branch lands" "landed spira/sp-land-doc" "$out"
git -C "$LREPO" fetch -q origin
git -C "$LREPO" merge-base --is-ancestor spira/sp-land-doc origin/main \
    && ok "and its document really reached origin/main" \
    || bad "a confined spike lands" "not an ancestor of origin/main"

beads "$(bead sp-land-poc "${SPIRA_SCOPE_LABEL},$SPIRA_SPIKE_LABEL,repo:home" task closed)"
land_branch sp-land-poc notes/spikes/answer2.md src/experiment.rs
# THE DEAD CLAIMANT'S NAME MUST COME OFF, and it is asserted on this path rather than assumed
# from the one next to it. `bd reopen` keeps the assignee and `bd ready --claim` skips an
# assigned bead while `bd ready` still lists it, so a reopen that forgets this puts the bead
# back in the graph wearing a name no aeon will ever claim past — visible, at P0, and dead.
# Every reopen site goes through bead_reopen for that reason; a refusal is a reopen like any
# other, and a new refusal path is exactly where the clearing gets left out.
bd -C "$SPIRA_DB" update sp-land-poc --assignee aeon-dead >/dev/null 2>&1
out="$(land)"
want   "an unconfined spike branch is refused" "reopened sp-land-poc" "$out"
nowant "and is not landed"                     "landed spira/sp-land-poc" "$out"
is     "and the bead is genuinely reopened"    open "$(status_of sp-land-poc)"
is     "and unassigned, so the next aeon can claim it" "" "$(assignee_of sp-land-poc)"
want   "and the note carries the offending path" "src/experiment.rs" \
       "$(bd -C "$SPIRA_DB" show sp-land-poc 2>/dev/null)"
git -C "$LREPO" fetch -q origin
git -C "$LREPO" merge-base --is-ancestor spira/sp-land-poc origin/main \
    && bad "the experiment stayed off main" "it was merged" \
    || ok "the experiment stayed off main"
# A SPIKE MAY LEAVE A BRANCH. The refusal must not reap the work it refused — the POC is the
# evidence, and deleting it is the one outcome worse than merging it.
git -C "$LREPO" rev-parse --verify -q spira/sp-land-poc >/dev/null \
    && ok "and the branch is still standing" \
    || bad "the branch survives a refusal" "it was deleted"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
