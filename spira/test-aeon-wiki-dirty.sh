#!/usr/bin/env bash
#
# test-aeon-wiki-dirty.sh — wiki_commit_paths (lib.sh) selects which of this session's OWN
#   wiki writes (wiki_write_paths, read from the transcript) an aeon may commit at exit:
#   only those still dirty now, and never wiki/tasks.md.
#
# THE DEFECT. An aeon that calls sop.sh synth or writes any wiki page and exits without
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
# defect: sp-4fl2e
# covers: spira/aeon.sh spira/lib.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-aeon-wiki-dirty.sh"

wcp() {   # wcp <write-paths> <dirty-paths> -> wiki_commit_paths's own output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; wiki_commit_paths "$2" "$3"' _ "$HERE" "$1" "$2" 2>/dev/null
}

# ===========================================================================================
echo
echo "T1: wiki_commit_paths <write-paths> <dirty-paths> — no aeon run, no bd"
# ===========================================================================================
out="$(wcp "$(printf 'wiki/notes/new.md')" "$(printf 'wiki/notes/new.md')")"
is "positive control: an own write that is still dirty is selected" "wiki/notes/new.md" "$out"

# CASE 2: pre-existing dirty file not among this session's own writes — never selected,
# regardless of it being dirty; only entries that are BOTH an own write AND dirty qualify.
out="$(wcp "$(printf 'wiki/notes/new.md')" "$(printf 'wiki/notes/preexist.md\nwiki/notes/new.md')")"
is "an own write among several dirty files: only the own write is selected" "wiki/notes/new.md" "$out"
nowant "a pre-existing dirty file the session never wrote is not selected" \
    "preexist.md" "$out"

# An own write that is NO LONGER dirty (reverted, or already committed by someone else)
# has nothing left to stage.
out="$(wcp "$(printf 'wiki/notes/reverted.md')" "$(printf 'wiki/notes/other.md')")"
is "an own write that is not currently dirty is not selected" "" "$out"

# CASE 4: wiki/tasks.md is excluded even when it IS this session's own write and dirty —
# the generated view is never authored by an aeon.
out="$(wcp "$(printf 'wiki/tasks.md')" "$(printf 'wiki/tasks.md')")"
is "wiki/tasks.md is never selected, even as an own dirty write" "" "$out"

out="$(wcp "$(printf 'wiki/tasks.md\nwiki/notes/new.md')" "$(printf 'wiki/tasks.md\nwiki/notes/new.md')")"
is "wiki/tasks.md is excluded from a mixed list; the real write still is not" \
   "wiki/notes/new.md" "$out"

# CASE 5: a concurrent actor's write — dirty, but never named by this session's own
# transcript — is never selected, no matter how it is ordered in the dirty list.
out="$(wcp "$(printf 'wiki/notes/mine.md')" "$(printf 'wiki/notes/concurrent-other.md\nwiki/notes/mine.md')")"
is "own write selected" "wiki/notes/mine.md" "$out"
nowant "a concurrent actor's write is never selected" "concurrent-other.md" "$out"

# Multiple own writes, all dirty: every one is selected, order preserved from write-paths.
out="$(wcp "$(printf 'wiki/a.md\nwiki/b.md\nwiki/c.md')" "$(printf 'wiki/c.md\nwiki/a.md\nwiki/b.md')")"
is "multiple own writes are all selected, in write-paths order" \
   "$(printf 'wiki/a.md\nwiki/b.md\nwiki/c.md')" "$out"

# No writes at all: nothing selected.
out="$(wcp "" "$(printf 'wiki/notes/concurrent-other.md')")"
is "no own writes at all: nothing selected" "" "$out"

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
cp "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/aeon.sh" "$HERE/wiki-commit.sh" "$SPIRA_HOME/"
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
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-wiki-dirty: aeon.sh has no SPIRA_AGENT injection point" >&2; exit 1; }
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
bash "$SPIRA_HOME/aeon.sh" builder >/dev/null 2>&1 || true
bead_status="$(bd -C "$SPIRA_DB" show "$b1" --json 2>/dev/null | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status","") if d else "")' 2>/dev/null)"
is "wiki-write: bead is closed" "closed" "$bead_status"
is "wiki-write: wiki checkout is clean after exit" "" \
    "$(git -C "$WIKI" diff --name-only HEAD 2>/dev/null)"
author="$(git -C "$WIKI" log --format="%ae" -1 -- "wiki/notes/sop-$b1.md" 2>/dev/null)"
want "wiki-write: wiki commit has aeon author" "aeon-" "$author"
msg="$(git -C "$WIKI" log --format="%s" -1 -- "wiki/notes/sop-$b1.md" 2>/dev/null)"
want "wiki-write: wiki commit message names the bead" "$b1" "$msg"

tl_summary
