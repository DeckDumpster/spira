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
# CHAMBER-INTEGRITY LINTS (placeholders, commands, brief clauses, fayth syntax, the
# `bd reopen` call-site rule) moved to test-lint-chamber.sh (UC-safety-fences-33): pure
# text reads that paid for this suite's Dolt fixture and bare git remote without needing
# either.
#
# defect: sp-qkf
# tier: T3
# covers: spira/confine.sh spira/landing.sh spira/chamber/* UC-safety-fences-32
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-spike.sh"

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
cp "$HERE/landing.sh" "$HERE/landing-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/confine.sh" "$SH/"
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
tl_summary
