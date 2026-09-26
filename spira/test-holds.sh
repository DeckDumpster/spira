#!/usr/bin/env bash
#
# test-holds.sh — holds.sh reports which OPEN beads have a branch touching a set of paths,
#   right now; bead_named_paths and render_holds_brief (lib.sh) turn that into the brief an
#   aeon reads before it edits.
#
# THE CASE THIS REPRODUCES. Three writers can add a case to the same few lines of a shared
# file on the same day without any existing tool noticing, because the duplicate-work guard
# keys on bead ids and held.sh/owned.sh/gate-touched.sh each answer a different question. The
# fix is a lookup keyed on the file itself: which open bead's branch already touches it.
#
# EVERY CHECK IS A PAIR (law-absence-needs-a-positive-control): the fixture proves a known
# holder is found before an empty result on an untouched path is trusted to mean anything.
#
# defect: sp-s9f30
# covers: spira/holds.sh spira/lib.sh spira/aeon.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-holds.sh"

# ===========================================================================================
echo
echo "T1: bead_named_paths <text> <repo> -> tracked paths named in the text"
# ===========================================================================================

BNP_REPO="$TMP/bnp-repo"
git init -q "$BNP_REPO"
mkdir -p "$BNP_REPO/spira"
printf 'x\n' > "$BNP_REPO/spira/batch.sh"
printf 'x\n' > "$BNP_REPO/spira/lib.sh"
git -C "$BNP_REPO" add -A
git -C "$BNP_REPO" -c user.email=t@t -c user.name=t commit -qm seed

bnp() {   # bnp <text> -> bead_named_paths's own output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; bead_named_paths "$2" "$3"' \
        _ "$HERE" "$1" "$BNP_REPO" 2>/dev/null
}

got="$(bnp "Three writers added a case to spira/batch.sh (around the trigger chain)")"
want "names the real path mentioned in prose"        "spira/batch.sh" "$got"
nowant "does not name a file that is not tracked"     "spira/nope.sh"  "$(bnp "also touches spira/nope.sh")"
nowant "a bead id is never read as a path"            "sp-2cvus"       "$(bnp "filed sp-2cvus, express lane 4/4")"
got2="$(bnp "touches spira/batch.sh and spira/lib.sh both")"
want "names the first of two real paths"  "spira/batch.sh" "$got2"
want "names the second of two real paths" "spira/lib.sh"   "$got2"
is   "no path-shaped text names nothing"  ""               "$(bnp "nothing file-shaped in here at all")"

# ===========================================================================================
echo
echo "T2: render_holds_brief <bead-id> <rc> <holds-output> -> the brief text"
# ===========================================================================================

rhb() {   # rhb <bead-id> <rc> <out> -> render_holds_brief's own output
    env -i PATH="$PATH" HOME="$TMP" LC_ALL=C.UTF-8 \
        SPIRA_CONF="$TMP/no.conf" SPIRA_RUN="$TMP/run" \
        bash -c '. "$1"/lib.sh; render_holds_brief "$2" "$3" "$4"' \
        _ "$HERE" "$1" "$2" "$3" 2>/dev/null
}

is "rc=0, no output -> no brief at all" "" "$(rhb sp-self 0 "")"

out="$(rhb sp-self 0 "$(printf 'tst-holder\tspira/batch.sh\n')")"
want "rc=0 with a hit names the holder"       "tst-holder"     "$out"
want "rc=0 with a hit names the path"         "spira/batch.sh" "$out"

self_only="$(rhb sp-self 0 "$(printf 'sp-self\tspira/other.sh\n')")"
is "a bead never reports holding against itself" "" "$self_only"

fail_out="$(rhb sp-self 1 "")"
want "rc!=0 renders a visible marker" "COULD NOT CHECK" "$fail_out"

# ===========================================================================================
echo
echo "T3: holds.sh <path>... — real git branches, real bd, positive control first"
# ===========================================================================================

# shellcheck disable=SC1090
. "$HERE/testdb.sh"
testdb_require test-holds
trap 'testdb_drop; rm -rf "$TMP"' EXIT INT TERM
testdb_up holds || {
    printf 'SKIP test-holds: testdb not available\n' >&2
    exit 77
}

export SPIRA_RUN="$TMP/run"; mkdir -p "$SPIRA_RUN"
export SPIRA_CONF="$TMP/no-such-conf"
export SPIRA_GOAL=sp-goal

REPO="$TMP/repo"
git init -q "$REPO"
git -C "$REPO" config user.email t@t; git -C "$REPO" config user.name t
mkdir -p "$REPO/spira"
printf 'root\n' > "$REPO/spira/batch.sh"
git -C "$REPO" add -A
git -C "$REPO" commit -qm root
BASE_BR="$(git -C "$REPO" symbolic-ref --short HEAD)"

# HOLDER: an open bead's branch edits the path.
git -C "$REPO" checkout -q -b spira/tst-holder
printf 'a new case\n' >> "$REPO/spira/batch.sh"
git -C "$REPO" commit -aqm "express lane case"
git -C "$REPO" checkout -q "$BASE_BR"

# CLOSED: a closed bead's branch also edits the path — must not be reported.
git -C "$REPO" checkout -q -b spira/tst-closed
printf 'closed work\n' >> "$REPO/spira/batch.sh"
git -C "$REPO" commit -aqm "closed work"
git -C "$REPO" checkout -q "$BASE_BR"

# ORPHAN: a branch with no bead in the store at all — skipped, must not fail the run.
git -C "$REPO" checkout -q -b spira/tst-orphan
printf 'orphan work\n' >> "$REPO/spira/batch.sh"
git -C "$REPO" commit -aqm "orphan work"
git -C "$REPO" checkout -q "$BASE_BR"

export SPIRA_REPO_MAP="$TMP/repo-map"
printf 'fixture | %s | queue | | |\n' "$REPO" > "$SPIRA_REPO_MAP"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-holder","title":"holder bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-closed","title":"closed bead","status":"closed","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

out="$(bash "$HERE/holds.sh" --repo fixture spira/batch.sh)"; rc=$?
wantrc "holder case exits 0"                     0 "$rc"
want   "holder case names the open holder"       "$(printf 'tst-holder\tspira/batch.sh')" "$out"
nowant "closed bead's branch is never reported"  "tst-closed" "$out"
nowant "orphan branch (no bead) is never reported, and does not fail the run" "tst-orphan" "$out"

# Positive control's complement: an untouched path is genuinely empty, not a broken check.
empty_out="$(bash "$HERE/holds.sh" --repo fixture spira/never-touched.sh)"; empty_rc=$?
wantrc "untouched path exits 0" 0 "$empty_rc"
is     "untouched path reports nothing" "" "$empty_out"

# ===========================================================================================
echo
echo "T4: bd unreadable — fail closed, never mistaken for a clean empty result"
# ===========================================================================================

REAL_BD="$(command -v bd)"
STUB_BD="$TMP/bd-stub"
cat > "$STUB_BD" <<EOF
#!/usr/bin/env bash
for a in "\$@"; do
    [ "\$a" = "tst-holder" ] && { printf 'bd-stub: simulated store failure\n' >&2; exit 1; }
done
exec "$REAL_BD" "\$@"
EOF
chmod +x "$STUB_BD"

unk_out="$(SPIRA_BD="$STUB_BD" bash "$HERE/holds.sh" --repo fixture spira/batch.sh 2>/dev/null)"
unk_rc=$?
if [ "$unk_rc" -ne 0 ]; then ok "holds.sh exits non-zero when bd is unreadable"
else bad "holds.sh exits non-zero when bd is unreadable" "got exit 0"; fi
# Exit 0 with empty stdout means "checked, found nothing"; this run's exit code is the ONLY
# thing telling it apart from that — its (also empty) stdout looks identical either way.
unset unk_out

# ===========================================================================================
echo
echo "T5: the caller that makes it fire — a claiming aeon's brief names the holder"
# ===========================================================================================

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

export SPIRA_HOME="$TMP/home"; mkdir -p "$SPIRA_HOME/chamber"
find "$HERE" -maxdepth 1 -name '*.sh' ! -name 'test-*.sh' -exec cp {} "$SPIRA_HOME/" \;
cp -r "$HERE/actors" "$SPIRA_HOME/" 2>/dev/null || true
cat > "$SPIRA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,$SPIRA_ASK_LABEL"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n<!-- task -->\n{{BEAD}}\n{{PARK}}\n' \
    > "$SPIRA_HOME/chamber/builder.md"

BIN="$TMP/bin"; mkdir -p "$BIN"; export SPIRA_AGENT="$BIN/claude" TMP
cat > "$BIN/claude" <<'SHIM'
#!/usr/bin/env bash
cat /dev/stdin > "$TMP/prompt"
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","num_turns":1}\n'
exit 0
SHIM
chmod +x "$BIN/claude"

testdb_reset
testdb_seed <<JSONL
{"id":"sp-goal","title":"goal","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-holder","title":"holder bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-claim","title":"claim bead","description":"a case in spira/batch.sh needs one more branch","status":"open","issue_type":"task","labels":["spira","plan","repo:fixture"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

rm -rf "$SPIRA_RUN/worktree"
"$SPIRA_HOME/aeon.sh" builder > "$TMP/out" 2>&1

if [ ! -s "$TMP/prompt" ]; then
    printf '# DEBUG aeon.sh output:\n' >&2
    sed 's/^/# /' "$TMP/out" >&2
fi

want "the claiming aeon's prompt names the holds section" "Files already in flight" "$(cat "$TMP/prompt" 2>/dev/null)"
want "and names the open bead already touching the file"  "tst-holder"              "$(cat "$TMP/prompt" 2>/dev/null)"

tl_summary
