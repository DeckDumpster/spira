#!/usr/bin/env bash
#
# test-aeon-resume-collision.sh — when the resume claim on the top resumable candidate loses
# its race, aeon.sh falls through to the NEXT resumable candidate rather than to the general
# claim's queue head.
#
# THE DEFECT (sp-3ntca). Aeons are summoned seconds apart and run the identical
# resume-candidate query, landing on the same first candidate. The old code tracked exactly
# one $resume_id and, on losing that claim, jumped straight to `bd ready --claim` — which has
# no idea a second resumable candidate exists and may return a bead with no prior work at
# all, or nothing, while a perfectly good resumable candidate sat one row down in the same
# query this aeon had already run.
#
# REAL bd, REAL claim, REAL race: the "another aeon already holds it" half of the story is
# produced by an actual `bd update --claim` from a different actor before aeon.sh ever runs,
# not a stub — the same reasoning test-aeon-resume.sh gives for driving the real binary
# (law-prefer-the-real-dependency). The one thing genuinely unknowable in advance is which of
# the two same-priority candidates `bd ready` will list first, since ordering among equal
# priorities is not a bd guarantee (aeon.sh says so where it builds this same query) — so the
# test asks bd the same question aeon.sh is about to ask, learns the order for itself, and
# pre-claims whichever row comes first. That is what makes the collision happen on every run
# instead of only on lucky ones.
#
# defect: sp-3ntca
# tier: T2
# covers: aeon/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-resume-collision
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT
trap 'testdb_drop; rm -rf "$TMP"; exit 130' INT
trap 'testdb_drop; rm -rf "$TMP"; exit 143' TERM
testdb_up aeonresumecollision || { echo "test-aeon-resume-collision: could not build a fixture database"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH

printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
# The model session is restricted (sp-v62vn); the shim is a fixture — testlib aeon_fixture_agent.
aeon_fixture_agent "$BIN/claude"
# sp-mve9i: the aeon reads its bead's state from the lifecycle row, never bd status; the
# shim's bd close is told to it in lifecycle terms (testlib.sh lc_aeon_mirror).
lc_aeon_mirror "$TMP/lcm"; export PATH="$TMP/lcm:$PATH"
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'resumed\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id - resumed"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","priority":0,"labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }

# ======================================================================================
echo
echo "TWO RESUMABLE P0 CANDIDATES, the one aeon.sh would try first already claimed:"
# ======================================================================================
testdb_reset
seed sp-cc-a open
seed sp-cc-b open
bd -C "$SPIRA_DB" set-state sp-cc-a "branch=spira/sp-cc-a" >/dev/null 2>&1 || true
bd -C "$SPIRA_DB" set-state sp-cc-b "branch=spira/sp-cc-b" >/dev/null 2>&1 || true

# Prior commits on BOTH branches, so aeon.sh's resume scan finds both resumable.
git -C "$REPO" fetch -q origin main 2>/dev/null
git -C "$REPO" checkout -q -B spira/sp-cc-a origin/main
printf 'a-work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-cc-a - prior attempt"
git -C "$REPO" checkout -q -B spira/sp-cc-b origin/main
printf 'b-work\n' >> "$REPO/f"; git -C "$REPO" commit -qam "sp-cc-b - prior attempt"
git -C "$REPO" checkout -q main

# Both are READY rows in the machine (the stand-in's list), so both are candidates.
cands="$(spira-lc list 2>/dev/null)"
[[ "$cands" == *'"sp-cc-a"'* && "$cands" == *'"sp-cc-b"'* ]] \
    && ok  "setup: both candidates are READY rows in the machine" \
    || bad "setup: both candidates are READY rows in the machine" "list=[$cands]"

# A PRE-CLAIM BEFORE THE AEON RUNS DOES NOT REPRODUCE THE RACE: the ready set lists only
# READY rows, so a bead claimed before the aeon's own read never appears as a candidate at
# all — the "collision" would never be attempted, only skipped, and the test would pass for
# the wrong reason. The race happens BETWEEN the aeon's read and its write, so it is
# manufactured at that exact point: the claim is a Claim event on the lifecycle row
# (sp-v62vn — bd's `update --claim` is nobody's claim any more), and a wrapper ahead of the
# stand-in on PATH, on the aeon's FIRST Claim, applies a Claim on the same row as a different
# actor first — so the aeon's own Claim is genuinely refused by the stand-in's own rule
# (Claim applies only to a READY/REWORK row), the same way it would lose to a second live
# aeon. Which id it was is recorded, so the suite learns the aeon's own order, never assumes it.
STOLEN_MARK="$TMP/stolen"
STEAL="$TMP/steal"; mkdir -p "$STEAL"
# The stand-in hands verbs it does not answer to the next spira-lc on PATH — which, with this
# wrapper first, would be the wrapper again; so the wrapper hands those to the tree's own
# spira-lc directly.
REAL_LC="$(PATH="${PATH//$TMP\/lcm:/}" command -v spira-lc)"
cat > "$STEAL/spira-lc" <<STUB
#!/usr/bin/env bash
case "\${1:-}" in
    show|list|event|create-bead|unclaim) ;;
    *) exec "$REAL_LC" "\$@" ;;
esac
if [ "\$1" = event ] && [[ "\$*" == *Claim* ]] && [ ! -f "$STOLEN_MARK" ]; then
    printf '%s\n' "\$3" > "$STOLEN_MARK"
    "$TMP/lcm/spira-lc" event bead "\$3" --expect READY --version 0 --actor aeon-other --kind '{"Claim":{}}' >/dev/null 2>&1
fi
exec "$TMP/lcm/spira-lc" "\$@"
STUB
chmod +x "$STEAL/spira-lc"

PATH="$STEAL:$PATH" run_aeon

first_id="$(cat "$STOLEN_MARK" 2>/dev/null)"
second_id="sp-cc-b"; [ "$first_id" = "sp-cc-b" ] && second_id="sp-cc-a"
row_of() { spira-lc show "$1" 2>/dev/null | python3 -c '
import sys, json
b = json.load(sys.stdin)["bead"]; print(b["state"], b.get("holder") or "-")' 2>/dev/null; }

is "loser ($first_id): left untouched — still held by the other aeon" "WORKING aeon-other" "$(row_of "$first_id")"
# The winner's session ran and closed it in bd: the stand-in reads that close as the
# builder's submit (the restricted session's own `work submit`, in lifecycle terms).
is "winner ($second_id): this aeon fell through to it, claimed it and finished it" "SUBMITTED -" "$(row_of "$second_id")"
claims="$(cat "$SPIRA_RUN/lc-claims.log" 2>/dev/null)"
want   "winner ($second_id): this aeon's claim applied"      "$second_id aeon-"      "$claims"
nowant "winner ($second_id): and it was never the other's"   "$second_id aeon-other" "$claims"
want "log: the collision on $first_id is named"      "$first_id"                                    "$(cat "$TMP/out")"
want "log: the fallback to the next candidate is named" "trying the next ranked candidate"        "$(cat "$TMP/out")"
want "log: it resumed $second_id, not the general claim head" "resuming $second_id"                  "$(cat "$TMP/out")"

tl_summary
