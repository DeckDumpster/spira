#!/usr/bin/env bash
#
# test-testdb-mode-lint.sh — the fence that refuses an unexplained server-mode testdb pin.
#
#   ./test-testdb-mode-lint.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# testdb-mode-lint.sh refuses a spira/test-*.sh that requests SPIRA_TESTDB_MODE=server
# without a `# testdb-mode: server — <reason>` header somewhere in the file. Server mode
# costs a median 110s per suite against ~5s for embedded; a request with no stated reason
# is indistinguishable from one copied from a suite that genuinely needed the real engine.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A fence that reports a clean tree is
# indistinguishable from one whose matcher never fires — a request line is planted in a
# scratch repository, the fence is required to name its file and line, the plant is
# withdrawn, and only then is the shipped tree's silence evidence of anything
# (law-absence-needs-a-positive-control).
#
# tier: T1
# covers: spira/testdb-mode-lint.sh spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-testdb-mode-lint.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

lint() { env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/testdb-mode-lint.sh" "$@" 2>&1; }
lint_at() {
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/testdb-mode-lint.sh" "$@" 2>&1
}

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — a whole scratch repository, because the walker's job is finding
# the files and no amount of --scan testing exercises that.
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/testdb-mode-lint.sh" "$ROOT/spira/testdb-mode-lint.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(lint_at "$ROOT")"; rc=$?
is   "no suites tracked refuses to report clean" "3" "$rc"
want "and says why"                              "refusing to report clean" "$out"

# Plant a request with no reason header on a known line: line 1 is the shebang, so the
# request is on line 2.
printf '#!/usr/bin/env bash\nexport SPIRA_TESTDB_MODE=server\n' > "$ROOT/spira/test-planted.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(lint_at "$ROOT")"; rc=$?
is   "SEEN RED: an unexplained server-mode request is refused" "1" "$rc"
want "and it names the file"                                   "spira/test-planted.sh" "$out"
want "and it names the line"                                   "spira/test-planted.sh:2" "$out"
want "and it names the escape hatch"                            "testdb-mode: server" "$out"

# ---------------------------------------------------------------------------------------
# A STATED REASON SILENCES IT. The plant stays in the tree so the fence must actually
# distinguish the two files, not merely exempt everything.
# ---------------------------------------------------------------------------------------
printf '#!/usr/bin/env bash\n# testdb-mode: server — needs real bd sql\nexport SPIRA_TESTDB_MODE=server\n' \
    > "$ROOT/spira/test-explained.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add explained suite"

out="$(lint_at "$ROOT")"
want   "plant is still reported while the explained suite is checked" "spira/test-planted.sh" "$out"
nowant "explained suite is not flagged"                               "test-explained.sh:" "$out"

# A suite that never requests server mode at all is unaffected.
printf '#!/usr/bin/env bash\necho embedded-only\n' > "$ROOT/spira/test-embedded.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add embedded-only suite"
out="$(lint_at "$ROOT")"
nowant "an embedded-only suite is not flagged" "test-embedded.sh" "$out"

# Withdrawn, and only now is a green reading evidence of anything.
git -C "$ROOT" rm -q spira/test-planted.sh
git -C "$ROOT" commit -q -m clean

out="$(lint_at "$ROOT")"; rc=$?
is   "GREEN AFTER: the same tree without the plant passes" "0" "$rc"
want "and reports how many suites it checked"              "clean" "$out"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
# In the gate container the worktree's .git FILE contains a gitdir: line that resolves
# to the host — a path that is not bind-mounted inside the container. git exits non-zero
# and testdb-mode-lint.sh exits 3 ("not a git repository"). Build a portable mirror from
# the real files, replacing the unreachable worktree gitdir with a plain git repo, and run
# lint_at against that instead. The set of files is identical; only the git plumbing
# differs.
out="$(lint)"; rc=$?
if [ "$rc" = 3 ]; then
    MIRROR="$TMP/shipped-mirror"
    mkdir -p "$MIRROR"
    SHIPPED="$(cd "$HERE/.." && pwd -P)"
    cp -a "$SHIPPED/." "$MIRROR/"
    rm -rf "$MIRROR/.git"
    git init -q -b main "$MIRROR"
    git -C "$MIRROR" config user.email t@t
    git -C "$MIRROR" config user.name t
    git -C "$MIRROR" add .
    git -C "$MIRROR" commit -q -m mirror
    out="$(lint_at "$MIRROR")"; rc=$?
fi
is   "every shipped suite passes" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

# ---------------------------------------------------------------------------------------
# --scan: matcher, one file at a time; exit 0 either way.
# ---------------------------------------------------------------------------------------
PROBE="$TMP/probe.sh"

printf '#!/usr/bin/env bash\nexport SPIRA_TESTDB_MODE=server\n' > "$PROBE"
out="$(lint --scan "$PROBE")"
want "--scan finds the unexplained request and reports its line" "2:" "$out"
want "and includes the matching text"                             "SPIRA_TESTDB_MODE=server" "$out"

# THE ESCAPE HATCH: a header anywhere in the file silences it.
printf '#!/usr/bin/env bash\n# testdb-mode: server — concurrency across processes\nexport SPIRA_TESTDB_MODE=server\n' > "$PROBE"
is   "a stated reason silences --scan" "" "$(lint --scan "$PROBE")"

# A header with no reason text does not count — it says nothing a reader could not
# already see from the request line itself.
printf '#!/usr/bin/env bash\n# testdb-mode: server —\nexport SPIRA_TESTDB_MODE=server\n' > "$PROBE"
want "an empty reason still flags the request" "SPIRA_TESTDB_MODE=server" "$(lint --scan "$PROBE")"

# A commented-out request is not a live request.
printf '#!/usr/bin/env bash\n# export SPIRA_TESTDB_MODE=server\n' > "$PROBE"
is   "a commented-out request is not flagged" "" "$(lint --scan "$PROBE")"

# An inline (non-exported) request is caught too.
printf '#!/usr/bin/env bash\nSPIRA_TESTDB_MODE=server testdb_up x\n' > "$PROBE"
want "an inline request without export is still caught" "SPIRA_TESTDB_MODE=server" "$(lint --scan "$PROBE")"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION. A fence nothing invokes is a file; this is the one property no amount
# of matcher testing can establish.
# ---------------------------------------------------------------------------------------
want "the gate names this fence" "spira/testdb-mode-lint.sh" "$(cat "$HERE/gate-spira.sh")"
is   "and it is executable"      "0" "$([ -x "$HERE/testdb-mode-lint.sh" ]; echo $?)"
tl_summary
