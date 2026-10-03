#!/usr/bin/env bash
#
# test-aeon-eviction-race.sh — a bead closed while its landstate carries a live batch-
#   eviction record is reopened; a gate-red, a stale tip or a record past the reopen cap is
#   not. The pure decision (reopen/stale/cap/none) is aeon::decide::eviction_reopen
#   (aeon/src/decide.rs) — eviction_reopen (lib.sh) retired dead (sp-j89pd, wave 4.2: zero
#   live callers); its T1 table is deleted with it. aeon's own block is only the side
#   effects (idempotence sidecar, the reopen/escalate calls), proved below by T3.
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
# tier: T2
# covers: aeon/src/* spira/lib.sh spira/conf.sh UC-aeon-execution-14
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-eviction-race.sh"

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
# conf.d IS COPIED IN (matching test-aeon-sweep.sh, test-aeon-world-stop.sh, ...): aeon's
# own in-process config registry (spira_config::resolve, aeon::conf::merge_resolved_config)
# derives conf.d from THIS --home and now REFUSES to start if it is missing (sp-1cdgq) --
# a --home with no conf.d used to resolve silently to nothing instead of refusing.
cp -r "$HERE/conf.d" "$SPIRA_HOME/"
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
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-eviction-race: aeon is not on PATH" >&2; exit 1; }

# spira-lc stub: `show` answers the lifecycle row the shim recorded in $SPIRA_RUN/lc-row.
cat > "$BIN/spira-lc" <<'STUB'
#!/usr/bin/env bash
case "$1" in
    show) if [ -f "$SPIRA_RUN/lc-row/$2" ]; then read -r st rs tip < "$SPIRA_RUN/lc-row/$2"
          printf '{"bead":{"bead_id":"%s","state":"%s","reason":"%s","tip":"%s","version":"1"},"delivery":null}\n' "$2" "$st" "$rs" "$tip"
          else printf '{"bead":{"bead_id":"%s","state":"WORKING","version":"1"},"delivery":null}\n' "$2"; fi ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$BIN/spira-lc"
export PATH="$BIN:$PATH"

# The shim commits, records a batch-ejected REWORK row at the real (post-commit) tip, then
# closes — reproducing the original race: the eviction is recorded, then the in-flight aeon
# closes the bead anyway.
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add -A && git -c user.email=a@a -c user.name=aeon commit -qm "$id — the work"
tip="$(git rev-parse HEAD)"
mkdir -p "$SPIRA_RUN/lc-row"; printf 'REWORK batch-ejected %s\n' "$tip" > "$SPIRA_RUN/lc-row/$id"
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
run_aeon() { rm -rf "$SPIRA_RUN/worktree"; aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1; }
field() { bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null | sed -n '/^[[{]/,$p' | python3 -c '
import sys,json
d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get(sys.argv[1]) or "")' "$2" 2>/dev/null; }
notes() { bd -C "$SPIRA_DB" show "$1" 2>/dev/null | tr '\n' ' '; }

testdb_reset; seed sp-er-1
run_aeon
is   "bead is open after eviction-race detection"     open "$(field sp-er-1 status)"
is   "and the claim is released"                       ""   "$(field sp-er-1 assignee)"
want "aeon log shows eviction-race reopen"             "REOPENED — closed with lifecycle state=REWORK" "$(cat "$TMP/out")"
want "and the reopen note names the cause"             "eviction-race" "$(notes sp-er-1)"

tl_summary
