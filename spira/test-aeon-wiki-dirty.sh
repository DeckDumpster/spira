#!/usr/bin/env bash
#
# test-aeon-wiki-dirty.sh — wiki_commit_paths selects which of this session's OWN wiki
#   writes (wiki_write_paths, read from the transcript) an aeon may commit at exit: only
#   those still dirty now, and never wiki/tasks.md.
#
# THE DEFECT. An aeon that calls `sop synth` or writes any wiki page and exits without
# committing leaves brain's shared checkout dirty. The next session to touch brain is
# refused by brain-guard's harness-checkout-clean arm and ends up committing someone
# else's work under its own author. The fix: aeon.sh commits wiki writes at exit,
# attributed to the aeon that produced them.
#
# SNAPSHOT-DIFF WAS TRIED AND REJECTED (sp-4fl2e): a before/after dirty-snapshot diff
# cannot tell "this session's own write" from "a concurrent actor's write that landed
# while this session was live" — both are just "dirty now, clean at the start". Only the
# transcript (wiki_write_paths) can. wiki_commit_paths is the intersection of what the
# transcript claims with what is actually dirty now, minus the generated view.
#
# RETIRED (wave 4.34, sp-27d3d): wiki_write_paths/wiki_commit_paths were lib.sh; both are
# ported to aeon::trace, called in-process from Run::wiki_commit (aeon/src/verdict.rs). The
# T1 table that used to call wiki_commit_paths through a bash sourcing shim (the positive
# control, the pre-existing-dirty-file case, the no-longer-dirty case, wiki/tasks.md alone
# and mixed, the concurrent-actor case, multiple own writes, and no writes at all) moved to
# wiki_commit_paths_selects_own_dirty_writes_never_tasks_md (aeon/src/trace.rs). T3 (below)
# is unaffected — it drives the real `aeon` binary end to end and does not care whether the
# family lives in bash or Rust.
#
# defect: sp-4fl2e
# tier: T3
# covers: aeon/src/* UC-aeon-execution-17
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-wiki-dirty.sh"

# ===========================================================================================
echo
echo "T3: one real aeon run proves the wiring — commit, author, message"
# ===========================================================================================
# testdb-mode: default (embedded).
unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN \
      TESTDB_MODE TESTDB_STARTED_SERVICE SPIRA_DB
# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-wiki-dirty
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonwikidirty || { echo "test-aeon-wiki-dirty: could not build fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

HARNESS_ORIGIN="$TMP/harness.git"; git init -q --bare -b main "$HARNESS_ORIGIN"
HARNESS="$TMP/harness"; git clone -q "$HARNESS_ORIGIN" "$HARNESS" 2>/dev/null
git -C "$HARNESS" config user.email t@t; git -C "$HARNESS" config user.name t
printf 'harness script v1\n' > "$HARNESS/seed.sh"
git -C "$HARNESS" add seed.sh
git -C "$HARNESS" commit -qm "seed harness"
git -C "$HARNESS" push -q origin main 2>/dev/null
export SPIRA_REPO="$HARNESS"

ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

WIKI_ORIGIN="$TMP/wiki.git"; git init -q --bare -b main "$WIKI_ORIGIN"
WIKI="$TMP/wiki"; git clone -q "$WIKI_ORIGIN" "$WIKI" 2>/dev/null
git -C "$WIKI" config user.email t@t; git -C "$WIKI" config user.name t
mkdir -p "$WIKI/wiki/notes"
printf 'wiki seed\n' > "$WIKI/wiki/seed.md"
printf '# tasks\n' > "$WIKI/wiki/tasks.md"
git -C "$WIKI" add wiki/seed.md wiki/tasks.md
git -C "$WIKI" commit -qm "seed wiki"
git -C "$WIKI" push -q origin main 2>/dev/null
export SPIRA_WIKI="$WIKI"

export SPIRA_HOME="$HARNESS/spira-home"; mkdir -p "$SPIRA_HOME/chamber"
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/wiki-commit.sh" "$SPIRA_HOME/"
cp -r "$HERE/conf.d" "$HERE/conf-gen.sh" "$SPIRA_HOME/"
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | push | origin/main | |\n' "$REPO" > "$SPIRA_REPO_MAP"
cat > "$SPIRA_HOME/chamber/builder.fayth" <<'FAYTH'
FAYTH_NAME=builder
FAYTH_LABELS="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,needs-operator"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP HARNESS WIKI ORIGIN REPO
command -v aeon >/dev/null 2>&1 \
    || { echo "test-aeon-wiki-dirty: aeon is not on PATH" >&2; exit 1; }
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf 'my work\n' >> f
git add f && git -c user.email=a@a -c user.name=aeon commit -qm "$id: the work"
printf '# SOP for %s\n' "$id" > "$WIKI/wiki/notes/sop-$id.md"
printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","name":"Write","input":{"file_path":"%s"}}]}}\n' \
    "$WIKI/wiki/notes/sop-$id.md"
bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
SHIM
chmod +x "$BIN/claude"

_lbl="${SPIRA_SCOPE_LABEL:+${SPIRA_SCOPE_LABEL},}${SPIRA_PLAN_LABEL:-plan},repo:fixture"

testdb_reset
b1="$(bd -C "$SPIRA_DB" create --title "test: wiki write" --type task -l "$_lbl" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b1" ] || { bad "bead created" "(bead-create failed)"; }
rm -rf "$SPIRA_RUN/worktree"
aeon --home "$SPIRA_HOME" builder >/dev/null 2>&1 || true
bead_status="$(bd -C "$SPIRA_DB" show "$b1" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status","") if d else "")' 2>/dev/null)"
# A task bead's close is converted to open+spira-submitted at teardown (sp-qsona): only the
# landing pass closes a work bead, so "open" here is the session's close having happened.
is "wiki-write: bead's close was converted to submitted" "open" "$bead_status"
want "wiki-write: carrying the submitted label" "spira-submitted" \
    "$(bd -C "$SPIRA_DB" show "$b1" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(",".join(d[0].get("labels") or []) if d else "")' 2>/dev/null)"
is "wiki-write: wiki checkout is clean after exit" "" \
    "$(git -C "$WIKI" diff --name-only HEAD 2>/dev/null)"
author="$(git -C "$WIKI" log --format="%ae" -1 -- "wiki/notes/sop-$b1.md" 2>/dev/null)"
want "wiki-write: wiki commit has aeon author" "aeon-" "$author"
msg="$(git -C "$WIKI" log --format="%s" -1 -- "wiki/notes/sop-$b1.md" 2>/dev/null)"
want "wiki-write: wiki commit message names the bead" "$b1" "$msg"

tl_summary
