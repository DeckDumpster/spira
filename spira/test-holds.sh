#!/usr/bin/env bash
#
# test-holds.sh — holds.sh reports which OPEN beads have a branch touching a set of paths,
#   right now; aeon turns that into the brief a claiming aeon reads before it edits.
#
# THE CASE THIS REPRODUCES. Three writers can add a case to the same few lines of a shared
# file on the same day without any existing tool noticing, because the duplicate-work guard
# keys on bead ids and held.sh/owned.sh/the suite selector each answer a different question. The
# fix is a lookup keyed on the file itself: which open bead's branch already touches it.
#
# EVERY CHECK IS A PAIR (law-absence-needs-a-positive-control): the fixture proves a known
# holder is found before an empty result on an untouched path is trusted to mean anything.
#
# defect: sp-s9f30
# covers: spira/holds.sh spira/lib.sh aeon/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

echo "test-holds.sh"

# ===========================================================================================
echo
echo "T1: holds.sh <path>... — real git branches, real bd, positive control first"
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
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-holder","title":"holder bead","status":"open","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-closed","title":"closed bead","status":"closed","issue_type":"task","labels":["spira","plan"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

out="$(holds.sh --repo fixture spira/batch.sh)"; rc=$?
wantrc "holder case exits 0"                     0 "$rc"
want   "holder case names the open holder"       "$(printf 'tst-holder\tspira/batch.sh')" "$out"
nowant "closed bead's branch is never reported"  "tst-closed" "$out"
nowant "orphan branch (no bead) is never reported, and does not fail the run" "tst-orphan" "$out"

# Positive control's complement: an untouched path is genuinely empty, not a broken check.
empty_out="$(holds.sh --repo fixture spira/never-touched.sh)"; empty_rc=$?
wantrc "untouched path exits 0" 0 "$empty_rc"
is     "untouched path reports nothing" "" "$empty_out"

# ===========================================================================================
echo
echo "T2: bd unreadable — fail closed, never mistaken for a clean empty result"
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

unk_out="$(SPIRA_BD="$STUB_BD" holds.sh --repo fixture spira/batch.sh 2>/dev/null)"
unk_rc=$?
if [ "$unk_rc" -ne 0 ]; then ok "holds.sh exits non-zero when bd is unreadable"
else bad "holds.sh exits non-zero when bd is unreadable" "got exit 0"; fi
# Exit 0 with empty stdout means "checked, found nothing"; this run's exit code is the ONLY
# thing telling it apart from that — its (also empty) stdout looks identical either way.
unset unk_out

# ===========================================================================================
echo
echo "T3: the caller that makes it fire — a claiming aeon's brief names the holder"
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

# FAYTH_LABELS reads $SPIRA_PLAN_LABEL/$SPIRA_SCOPE_LABEL, not a literal "plan" — a fixture
# bead must carry whatever this environment actually configured, or "nothing ready to claim"
# is indistinguishable from the wiring being broken.
_t5_lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\""

testdb_reset
testdb_seed <<JSONL
{"id":"sp-epic","title":"epic","status":"open","issue_type":"epic","labels":["spira"],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-holder","title":"holder bead","status":"open","issue_type":"task","labels":[$_t5_lbl],"updated_at":"2026-09-20T00:00:00Z"}
{"id":"tst-claim","title":"claim bead","description":"a case in spira/batch.sh needs one more branch","status":"open","issue_type":"task","labels":[$_t5_lbl,"repo:fixture"],"updated_at":"2026-09-20T00:00:00Z"}
JSONL

rm -rf "$SPIRA_RUN/worktree"
aeon --home "$SPIRA_HOME" builder > "$TMP/out" 2>&1
aeon_rc=$?

# A starved or killed run leaves no prompt at all; say so instead of reporting two content mismatches.
if [ -s "$TMP/prompt" ]; then ok "the shim agent captured a prompt"
else bad "the shim agent captured a prompt" "aeon rc=$aeon_rc, no prompt written; aeon output: $(tail -c 600 "$TMP/out" 2>/dev/null)"; fi

want "the claiming aeon's prompt names the holds section" "Files already in flight" "$(cat "$TMP/prompt" 2>/dev/null)"
want "and names the open bead already touching the file"  "tst-holder"              "$(cat "$TMP/prompt" 2>/dev/null)"

tl_summary
