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
# covers: spira/aeon.sh
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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$SPIRA_HOME/"
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
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }

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

# THE SAME QUESTION aeon.sh is about to ask (READY_ARGS, unfiltered by branch state) —
# asked here so the test learns bd's own row order instead of assuming one, since ordering
# among equal-priority rows is exactly the thing aeon.sh's own comments say bd never promised.
order_ids="$(bd -C "$SPIRA_DB" ready --limit 0 --exclude-type epic,event -u --label plan --json 2>/dev/null \
    | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
for i in d:
    if i["id"] in ("sp-cc-a", "sp-cc-b"): print(i["id"])')"
first_id="$(printf '%s\n' "$order_ids" | head -1)"
second_id="sp-cc-b"; [ "$first_id" = "sp-cc-b" ] && second_id="sp-cc-a"
[ -n "$first_id" ] && [ "$first_id" != "$second_id" ] \
    && ok  "setup: both candidates present and ordered ($first_id then $second_id)" \
    || bad "setup: both candidates present and ordered" "order_ids=[$order_ids]"

# A PRE-CLAIM BEFORE aeon.sh RUNS DOES NOT REPRODUCE THE RACE: `bd ready` lists only
# UNCLAIMED beads, so a bead claimed before aeon.sh's own resume query runs never appears
# as a candidate at all — the "collision" would never be attempted, only skipped, and the
# test would pass for the wrong reason (the loser is never a candidate, not a candidate
# that lost). The race this bug is about happens BETWEEN aeon.sh's own read and its own
# write, so it has to be manufactured at that exact point.
#
# SPIRA_BD WRAPS THE REAL bd. Every call passes straight through to the real embedded
# binary except the one this test cares about: the moment aeon.sh itself asks to claim
# $first_id, the wrapper claims it FIRST, as a different actor, using the real bd's own
# atomicity — so aeon.sh's own subsequent claim on that id genuinely loses, the same way
# it would against a second live aeon, not a fabricated empty response.
REAL_BD="$(command -v bd)"
BIN_BD="$TMP/bin"; mkdir -p "$BIN_BD"
STOLEN_MARK="$TMP/stolen"
cat > "$BIN_BD/bd" <<STUB
#!/usr/bin/env bash
is_target=0 has_update=0 has_claim=0
for a in "\$@"; do
    [ "\$a" = "update" ] && has_update=1
    [ "\$a" = "$first_id" ] && is_target=1
    [ "\$a" = "--claim" ] && has_claim=1
done
if [ "\$has_update" = 1 ] && [ "\$is_target" = 1 ] && [ "\$has_claim" = 1 ] && [ ! -f "$STOLEN_MARK" ]; then
    touch "$STOLEN_MARK"
    BEADS_ACTOR="aeon-other" "$REAL_BD" -C "$SPIRA_DB" update "$first_id" --claim >/dev/null 2>&1
fi
exec "$REAL_BD" "\$@"
STUB
chmod +x "$BIN_BD/bd"
export SPIRA_BD="$BIN_BD/bd"

run_aeon

first_status="$(bd -C "$SPIRA_DB" show "$first_id" --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(d.get("status"))' 2>/dev/null)"
second_status="$(bd -C "$SPIRA_DB" show "$second_id" --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(d.get("status"))' 2>/dev/null)"

is "loser ($first_id): left untouched — still held by the other aeon" "in_progress" "$first_status"
# A task bead's close is converted to open + spira-submitted at teardown (sp-qsona): only
# the landing pass closes a work bead.
second_labels="$(bd -C "$SPIRA_DB" show "$second_id" --json 2>/dev/null | python3 -c '
import sys, json
d = json.load(sys.stdin); d = d[0] if isinstance(d, list) else d
print(",".join(d.get("labels") or []))' 2>/dev/null)"
is "winner ($second_id): this aeon fell through to it and closed it (converted to submitted)" "open" "$second_status"
case ",$second_labels," in
    *",spira-submitted,"*) ok "winner ($second_id): carrying the submitted label" ;;
    *) bad "winner ($second_id): carrying the submitted label" "labels=[$second_labels]" ;;
esac
want "log: the collision on $first_id is named"      "$first_id"                                    "$(cat "$TMP/out")"
want "log: the fallback to the next candidate is named" "trying the next resumable candidate"        "$(cat "$TMP/out")"
want "log: it resumed $second_id, not the general claim head" "resuming $second_id"                  "$(cat "$TMP/out")"

tl_summary
