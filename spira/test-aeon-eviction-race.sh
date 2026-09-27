#!/usr/bin/env bash
#
# test-aeon-eviction-race.sh — a bead closed while its landstate carries a live batch-
#   eviction record is reopened; a gate-red, a stale tip or a record past the reopen cap is
#   not. eviction_reopen (lib.sh) is the pure decision (reopen/stale/cap/none); aeon.sh's own
#   block is only the side effects (idempotence sidecar, the reopen/escalate calls).
#
# THE ORIGINAL DEFECT (sp-htw4r). A batch eviction writes landstate=RED/EJECTED and calls
# bead_reopen. An aeon still in flight does not see the reopen — it closes the bead after
# the reopen (the close succeeds because the bead is now open). aeon.sh's exit checks read
# st=closed and committed=yes, all guards pass, and the aeon exits without reopening. The
# bead lands closed with a RED landstate: no queue mechanism retrieves it.
#
# THE REGRESSION (sp-ygvu0). skip RED reason=gate (normal path); skip when the record tip is
# older than the current branch tip (session pushed past the eviction).
#
# THE UNBOUNDED LOOP (sp-r1501). A bead whose landstate record never gets recertified would
# reopen on every pass forever. SPIRA_EVICTION_ESCALATE_AT caps it: at that many prior
# eviction-race requeues, escalate to the operator instead of reopening again.
#
# NOT REOPENING IS NOT "STAYS CLOSED" (sp-qsona). A none/stale/cap verdict only means the
# eviction-race guard itself does not fire; cleanup()'s later submitted conversion still
# applies to a work bead, so an e2e run ends open+spira-submitted rather than closed.
#
# defect: sp-htw4r sp-ygvu0 sp-r1501
# covers: spira/aeon.sh spira/lib.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-eviction-race.sh"

# ===========================================================================================
echo
echo "T1: eviction_reopen <land-state> <cur-tip> <recent> -> reopen|stale|cap|none"
# ===========================================================================================
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): row 3 (reopen) proves the
# function fires at all before the "none" rows below are trusted to mean anything.

evr() {   # evr <land-state-string> <cur-tip> <recent> [escalate_at] -> eviction_reopen's output
    local ls="$1" cur="$2" recent="$3" esc="${4:-}"
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        ${esc:+SPIRA_EVICTION_ESCALATE_AT="$esc"} \
        bash -c '. "$1"/lib.sh; eviction_reopen "$2" "$3" "$4"' _ "$HERE" "$ls" "$cur" "$recent" 2>/dev/null
}

# name|land-state|cur-tip|recent|escalate_at (empty = default 3)|want
ROWS=(
    "(a) RED reason=gate at current tip stays closed|RED tipX 111 gate|tipX|0||none"
    "(b) RED reason=ejected at a stale tip stays closed|RED tipOld 111 ejected|tipNew|0||stale"
    "(c) RED reason=ejected at current tip reopens|RED tipX 111 ejected|tipX|0||reopen"
    "EJECTED at current tip reopens (no reason needed)|EJECTED tipX 111|tipX|0||reopen"
    "EJECTED with no recorded tip (none) is never stale|EJECTED none 111|tipX|0||reopen"
    "no landstate at all stays closed|EMPTY|tipX|0||none"
    "CERTIFIED stays closed (positive control)|CERTIFIED tipX 111 certified|tipX|0||none"
    "RED no-rebase@<sha> stays closed (landing.sh owns this)|RED tipX 111 no-rebase@deadbeef|tipX|0||none"
    "below the cap (non-default escalate_at=2) still reopens|RED tipX 111 ejected|tipX|1|2|reopen"
    "at the cap (non-default escalate_at=2) escalates instead|RED tipX 111 ejected|tipX|2|2|cap"
)

for row in "${ROWS[@]}"; do
    IFS='|' read -r name ls cur recent esc want <<<"$row"
    [ "$ls" = "EMPTY" ] && ls=""
    got="$(evr "$ls" "$cur" "$recent" "$esc")"
    is "$name" "$want" "$got"
done

# ===========================================================================================
echo
echo "T3: one aeon run proves the wiring (reopen), including the idempotence sidecar"
# ===========================================================================================
# testdb-mode: default (embedded) — the cap/stale/none decisions above are now pure-function
# rows and no longer need bd sql event seeding, so this suite no longer requires server mode.

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-eviction-race
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonevictionrace || {
    printf 'SKIP test-aeon-eviction-race: testdb not available\n' >&2
    exit 77
}
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed; git -C "$REPO" push -q origin main 2>/dev/null

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
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
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-eviction-race: aeon.sh has no SPIRA_AGENT injection point" >&2; exit 1; }

# The shim commits, writes landstate=RED reason=ejected at the real (post-commit) tip, then
# closes — reproducing the original race: the eviction is recorded, then the in-flight aeon
# closes the bead anyway.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
tip="$(git rev-parse HEAD)"
printf 'RED %s %s ejected\n' "$tip" "$(date +%s)" > "$SPIRA_RUN/landstate/$id"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":3}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

seed() {
    local _lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"%s","issue_type":"task","labels":[%s],"updated_at":"2026-09-04T00:00:00Z"}\n' \
        "$1" "${2:-open}" "$_lbl" | testdb_seed
}
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; "$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1; }
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
notes() { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }

mkdir -p "$SPIRA_RUN/landstate"

testdb_reset; seed sp-er-1
run_aeon
is   "bead is open after eviction-race detection"     open "$(field sp-er-1 status)"
is   "and the claim is released"                       ""   "$(field sp-er-1 assignee)"
want "aeon log shows eviction-race reopen"             "REOPENED — closed with landstate=RED" "$(cat "$TMP/out")"
want "and the reopen note names the cause"             "eviction-race" "$(notes sp-er-1)"

tl_summary
