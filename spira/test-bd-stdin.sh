#!/usr/bin/env bash
#
# test-bd-stdin.sh — the fence that refuses a bare "-" body on bd note / bd create (UC-05).
#
#   ./test-bd-stdin.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# bd-stdin-lint.sh refuses a bare-dash body: `bd note <id> - <<EOF` and
# `bd create ... -d - <<EOF` store the literal "-" and discard the heredoc that follows —
# six beads shipped with a dash where their body or notes should be (sp-j5z3). The check
# used to live inside this suite as two separate grep -r passes over spira/ and chamber/
# (one per pattern); it is now the fence gate-spira.sh runs directly, in one pass, and this
# suite is its positive control (UC-dispatch-05, T0).
#
# THE POSITIVE CONTROL IS FIRST. A fence that reports a clean tree is indistinguishable
# from one whose matcher never fires — a bad pattern is planted in a scratch repository,
# the fence is required to name its file and line, the plant is withdrawn, and only then is
# the shipped tree's silence evidence of anything (law-absence-needs-a-positive-control).
#
# defect: sp-j5z3
# tier: T0
# covers: spira/bd-stdin-lint.sh spira/gate-spira.sh UC-dispatch-05
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

lint() { env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/bd-stdin-lint.sh" "$@" 2>&1; }
lint_at() {
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/bd-stdin-lint.sh" "$@" 2>&1
}

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — a whole scratch repository, because the walker's job is finding
# the files, and one combined pass over both patterns is what replaced the double scan.
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira" "$ROOT/chamber"
cp "$HERE/bd-stdin-lint.sh" "$ROOT/spira/bd-stdin-lint.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(lint_at "$ROOT")"; rc=$?
is   "no files tracked refuses to report clean" "3" "$rc"
want "and says why"                              "refusing to report clean" "$out"

# Plant both bad shapes, in spira/ and chamber/, so the walker proves it covers both trees.
printf '#!/usr/bin/env bash\nbd -C /db note sp-abc - <<%sNOTE%s\nsome text\nNOTE\n' "'" "'" > "$ROOT/spira/planted.sh"
printf 'bd -C {{DB}} create "title" -d - -l plan <<%sBODY%s\nprose here\nBODY\n' "'" "'" > "$ROOT/chamber/planted.md"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(lint_at "$ROOT")"; rc=$?
is   "SEEN RED: a bare-dash note is refused"      "1"                          "$rc"
want "and it names the spira/ file and line"      "spira/planted.sh:2"        "$out"
want "and it names the chamber/ file and line"    "chamber/planted.md:1"      "$out"
want "and it names the escape hatch"              "--stdin"                   "$out"

# ---------------------------------------------------------------------------------------
# THE CORRECT FORMS ARE SILENT, even sitting right next to the plants.
# ---------------------------------------------------------------------------------------
cat >> "$ROOT/spira/planted.sh" <<'EOF'
bd -C /db note sp-abc --stdin <<'NOTE'
clean
NOTE
bd -C /db create "title" --body-file - -l plan <<'BODY'
clean
BODY
EOF
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add clean usage beside the plants"

out="$(lint_at "$ROOT")"
want   "the plants are still reported"   "spira/planted.sh:2" "$out"
nowant "the --stdin form is not flagged" "note sp-abc --stdin" "$out"
nowant "the --body-file form is not flagged" "body-file - -l plan" "$out"

# A commented-out example (documentation showing the bad shape) is not a live invocation.
printf '# bd -C /db note sp-abc - <<EOF\n' > "$ROOT/spira/doc.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add commented-out example"
out="$(lint_at "$ROOT")"
nowant "a commented-out example is not flagged" "spira/doc.sh" "$out"

# Withdrawn, and only now is a green reading evidence of anything.
git -C "$ROOT" rm -q spira/planted.sh chamber/planted.md
git -C "$ROOT" commit -q -m clean

out="$(lint_at "$ROOT")"; rc=$?
is   "GREEN AFTER: the same tree without the plants passes" "0" "$rc"
want "and reports how many files it checked"                "clean" "$out"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
# In the gate container the worktree's .git FILE resolves to the host, which is not
# bind-mounted inside the container: git exits non-zero and bd-stdin-lint.sh exits 3.
# Build a portable mirror from the real files and run lint_at against that instead — the
# set of files is identical; only the git plumbing differs.
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
is   "the shipped tree passes" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

# ---------------------------------------------------------------------------------------
# --scan: matcher, one file at a time; exit 0 either way; one pass covers both shapes.
# ---------------------------------------------------------------------------------------
PROBE="$TMP/probe.sh"

printf '#!/usr/bin/env bash\nbd -C /db note sp-abc - <<%sNOTE%s\ntext\nNOTE\n' "'" "'" > "$PROBE"
out="$(lint --scan "$PROBE")"
want "--scan finds the bare-dash note and reports its line" "2:" "$out"
want "and includes the matching text"                        "note sp-abc -" "$out"

printf '%s\n' 'bd -C {{DB}} create "title" -d - -l plan <<BODY' > "$PROBE"
out="$(lint --scan "$PROBE")"
want "--scan finds the bare-dash create too, same pass" "create \"title\" -d -" "$out"

printf '#!/usr/bin/env bash\nbd -C /db note sp-abc --stdin <<%sNOTE%s\ntext\nNOTE\n' "'" "'" > "$PROBE"
is   "--scan is silent on --stdin usage" "" "$(lint --scan "$PROBE")"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION. A fence nothing invokes is a file; this is the one property no amount
# of matcher testing can establish.
# ---------------------------------------------------------------------------------------
want "the gate names this fence" "spira/bd-stdin-lint.sh" "$(cat "$HERE/gate-spira.sh")"
is   "and it is executable"      "0" "$([ -x "$HERE/bd-stdin-lint.sh" ]; echo $?)"

tl_summary
