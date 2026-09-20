#!/usr/bin/env bash
#
# test-aeon-wiki-dirty.sh — an aeon that writes to SPIRA_WIKI commits those writes at exit.
#
#   ./test-aeon-wiki-dirty.sh
#
# THE DEFECT. An aeon that calls sop.sh synth or writes any wiki page and exits without
# committing leaves brain's shared checkout dirty. The next session to touch brain is
# refused by brain-guard's harness-checkout-clean arm and ends up committing someone
# else's work under its own author. The fix: aeon.sh commits wiki writes at exit,
# attributed to the aeon that produced them.
#
# FOUR CASES:
#   1. (positive control) aeon writes a wiki page, exits → page is committed with
#      aeon author, wiki checkout is clean.
#   2. Wiki page was dirty BEFORE the session started → aeon does NOT commit it (not
#      its work); wiki stays dirty but attribution is preserved.
#   3. SPIRA_WIKI not set → no failure; bead closes normally.
#   4. wiki/tasks.md is dirty → NOT committed by the aeon (generated view, excluded).
#
# SEEN RED FIRST. Case 1 is the positive control: a shim that writes a wiki page and
# exits without committing it. Before the fix, the wiki is left dirty and the test
# fails (the page is not committed). After the fix, the harness commits it.
#
# Driven through the REAL aeon.sh against a real bd on a throwaway fixture.
#
# covers: spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

unset TESTDB_SHARED TESTDB_NAME TESTDB_DIR TESTDB_BASELINE TESTDB_BIN \
      TESTDB_MODE TESTDB_STARTED_SERVICE SPIRA_DB

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-aeon-wiki-dirty
TMP="$(mktemp -d)"
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up aeonwikidirty || { echo "test-aeon-wiki-dirty: could not build fixture"; exit 1; }
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# ---- harness repo (SPIRA_REPO) --------------------------------------------------------
HARNESS_ORIGIN="$TMP/harness.git"; git init -q --bare -b main "$HARNESS_ORIGIN"
HARNESS="$TMP/harness"; git clone -q "$HARNESS_ORIGIN" "$HARNESS" 2>/dev/null
git -C "$HARNESS" config user.email t@t; git -C "$HARNESS" config user.name t
printf 'harness script v1\n' > "$HARNESS/seed.sh"
git -C "$HARNESS" add seed.sh
git -C "$HARNESS" commit -qm "seed harness"
git -C "$HARNESS" push -q origin main 2>/dev/null
export SPIRA_REPO="$HARNESS"

# ---- bead target repo (separate from SPIRA_REPO) --------------------------------------
ORIGIN="$TMP/origin.git"; git init -q --bare -b main "$ORIGIN"
REPO="$TMP/repo"; git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
printf 'seed\n' > "$REPO/f"
git -C "$REPO" add f; git -C "$REPO" commit -qm seed
git -C "$REPO" push -q origin main 2>/dev/null

# ---- wiki repo (SPIRA_WIKI) -----------------------------------------------------------
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

# ---- harness setup --------------------------------------------------------------------
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

# ---- shim: stands in for the model ---------------------------------------------------
# SPIRA_AGENT must be set; conf.sh replaces $PATH so a PATH shim runs the real model.
BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP HARNESS WIKI ORIGIN REPO
grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
    || { echo "test-aeon-wiki-dirty: aeon.sh has no SPIRA_AGENT injection point" >&2; exit 1; }

# Shim behaviour driven by $TMP/shim-mode:
#   wiki-write      — write a wiki page and close the bead, without committing the wiki
#   wiki-preexist   — wiki page already dirty; write a DIFFERENT wiki page and close
#   no-wiki-write   — commit bead work only; do not touch the wiki
#   wiki-tasks-only — dirty wiki/tasks.md only; close the bead
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
id="$(sed -n 's/^work \(sp-[a-z0-9-]*\) .*/\1/p' "$TMP/prompt" | head -1)"
printf '%s' "$id" > "$TMP/last-bead"

# Commit the bead's own work in the bead repo.
printf 'my work\n' >> f
git add f && git -c user.email=a@a -c user.name=aeon commit -qm "$id: the work"

case "$(cat "$TMP/shim-mode" 2>/dev/null)" in
    wiki-write)
        # Write a wiki page in SPIRA_WIKI WITHOUT committing it.
        # This is the positive control: the harness must commit it for us.
        printf '# SOP for %s\n' "$id" > "$WIKI/wiki/notes/sop-$id.md"
        ;;
    wiki-preexist)
        # wiki/notes/preexist.md is already dirty (written before the session).
        # Write a NEW page as well. Only the new page should be committed.
        printf '# New page for %s\n' "$id" > "$WIKI/wiki/notes/new-$id.md"
        ;;
    wiki-tasks-only)
        # Only dirty wiki/tasks.md — the generated view. Must NOT be committed.
        printf '# tasks regenerated\n' >> "$WIKI/wiki/tasks.md"
        ;;
    *)
        # no-wiki-write: nothing to write in wiki
        ;;
esac

bd -C "$SPIRA_DB" close "$id" --reason "done" >/dev/null 2>&1
SHIM
chmod +x "$BIN/claude"

bead_status() {
    bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c 'import sys,json; d=json.load(sys.stdin); d=d if isinstance(d,list) else [d]; print(d[0].get("status","") if d else "")' 2>/dev/null
}
commit_author_of() {  # commit_author_of <git-dir> <file>
    git -C "$1" log --format="%ae" -1 -- "$2" 2>/dev/null
}
wiki_clean() {
    [ -z "$(git -C "$WIKI" diff --name-only HEAD 2>/dev/null)" ]
}
poison_bead() {
    bd -C "$SPIRA_DB" label add "$1" spira-poison >/dev/null 2>&1 || true
}

# ============================================================
echo
echo "CASE 1 (positive control): aeon writes a wiki page — must be committed with aeon author:"
echo "-----------------------------------------------------------------------"
# SEEN RED FIRST. Without the wiki commit check in aeon.sh, this case produces
# an uncommitted wiki/notes/sop-<id>.md and fails the assertions below.
printf 'wiki-write' > "$TMP/shim-mode"
b1="$(bd -C "$SPIRA_DB" create --title "test: wiki write" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b1" ] || { bad "case 1 bead created" "(bead-create failed)"; true; }
bash "$SPIRA_HOME/aeon.sh" builder >/dev/null 2>&1 || true
is "wiki-write: bead is closed" "closed" "$(bead_status "$b1")"
is "wiki-write: wiki checkout is clean after exit" "" \
    "$(git -C "$WIKI" diff --name-only HEAD 2>/dev/null)"
_author1="$(commit_author_of "$WIKI" "wiki/notes/sop-$b1.md")"
want "wiki-write: wiki commit has aeon author" "aeon-" "$_author1"
_msg1="$(git -C "$WIKI" log --format="%s" -1 -- "wiki/notes/sop-$b1.md" 2>/dev/null)"
want "wiki-write: wiki commit message names the bead" "$b1" "$_msg1"
poison_bead "$b1"

# ============================================================
echo
echo "CASE 2: pre-existing dirty wiki file — aeon must NOT commit it:"
echo "-----------------------------------------------------------------------"
# wiki/notes/preexist.md was dirty before the session started.
# The shim also writes a NEW page. Only the new page should be committed.
printf 'pre-existing content\n' > "$WIKI/wiki/notes/preexist.md"
git -C "$WIKI" add "wiki/notes/preexist.md" 2>/dev/null || true
# Unstage it: we want it tracked-but-dirty (modified after last commit).
# Actually we want it as an untracked modification. Let's do: create the file,
# commit it, then modify it again.
git -C "$WIKI" commit -qm "add preexist.md" 2>/dev/null || true
git -C "$WIKI" push -q 2>/dev/null || true
printf 'modified content\n' >> "$WIKI/wiki/notes/preexist.md"

printf 'wiki-preexist' > "$TMP/shim-mode"
b2="$(bd -C "$SPIRA_DB" create --title "test: preexist wiki" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b2" ] || { bad "case 2 bead created" "(bead-create failed)"; true; }
bash "$SPIRA_HOME/aeon.sh" builder >/dev/null 2>&1 || true
is "preexist: bead is closed" "closed" "$(bead_status "$b2")"
# preexist.md was dirty before the session; it must remain uncommitted.
_preexist_dirty="$(git -C "$WIKI" diff --name-only HEAD -- wiki/notes/preexist.md 2>/dev/null)"
is "preexist: pre-existing dirty file NOT committed" "wiki/notes/preexist.md" "$_preexist_dirty"
# The new page the aeon wrote MUST be committed.
is "preexist: new wiki page committed" "" \
    "$(git -C "$WIKI" diff --name-only HEAD -- "wiki/notes/new-$b2.md" 2>/dev/null)"
# Clean up the preexist file.
git -C "$WIKI" checkout -q -- "wiki/notes/preexist.md" 2>/dev/null || true
poison_bead "$b2"

# ============================================================
echo
echo "CASE 3: SPIRA_WIKI not set — aeon must close normally:"
echo "-----------------------------------------------------------------------"
printf 'no-wiki-write' > "$TMP/shim-mode"
b3="$(bd -C "$SPIRA_DB" create --title "test: no wiki" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b3" ] || { bad "case 3 bead created" "(bead-create failed)"; true; }
SPIRA_WIKI="" bash "$SPIRA_HOME/aeon.sh" builder >/dev/null 2>&1 || true
is "no-wiki: bead is closed" "closed" "$(bead_status "$b3")"
poison_bead "$b3"

# ============================================================
echo
echo "CASE 4: wiki/tasks.md dirty — must NOT be committed by the aeon:"
echo "-----------------------------------------------------------------------"
printf 'wiki-tasks-only' > "$TMP/shim-mode"
b4="$(bd -C "$SPIRA_DB" create --title "test: tasks.md dirty" --type task \
        -l "${SPIRA_SCOPE_LABEL:+$SPIRA_SCOPE_LABEL,}${SPIRA_PLAN_LABEL:-plan},repo:fixture" 2>/dev/null | grep -oE 'sp-[a-z0-9-]+')"
[ -n "$b4" ] || { bad "case 4 bead created" "(bead-create failed)"; true; }
bash "$SPIRA_HOME/aeon.sh" builder >/dev/null 2>&1 || true
is "tasks-only: bead is closed" "closed" "$(bead_status "$b4")"
# tasks.md must still be dirty after the aeon exits.
_tasks_dirty="$(git -C "$WIKI" diff --name-only HEAD -- wiki/tasks.md 2>/dev/null)"
is "tasks-only: wiki/tasks.md NOT committed by aeon" "wiki/tasks.md" "$_tasks_dirty"
# Clean up.
git -C "$WIKI" checkout -q -- "wiki/tasks.md" 2>/dev/null || true
poison_bead "$b4"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
