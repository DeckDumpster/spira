#!/usr/bin/env bash
#
# test-literal-lint.sh — the fence that refuses configured-name literals outside declaration files.
#
#   ./test-literal-lint.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# literal-lint.sh refuses source files that contain a label, status or type name as a
# literal — names that are declared in schema.sh and read everywhere else through
# SPIRA_*_LABEL env vars set by conf.sh. A literal that bypasses these can disagree with
# the declaration when an operator changes the default: lib.sh:117 once grepped
# "needs-ryan" while lib.sh:221 read "${SPIRA_ASK_LABEL:-needs-ryan}", and the code
# default was needs-operator — so the destructive-procedure fence refused every legitimate
# halting bead on a default install.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A fence that reports a clean tree is
# indistinguishable from one whose matcher never fires, an empty glob, or a bad path —
# and the reassuring reading is what all three give. A literal is planted in a scratch
# repository, the fence is required to name its file and line, the plant is withdrawn,
# and only then is the shipped tree's silence evidence of anything
# (law-absence-needs-a-positive-control).
#
# BOTH MODES: --scan (one file) and the whole-tree walk (scratch git repo). They fail
# differently: --scan shows the matcher fires; the walk shows the root is resolved and
# files are actually traversed.
#
# covers: spira/literal-lint.sh spira/gate-spira.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-literal-lint.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

lint() { env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$HERE/literal-lint.sh" "$@" 2>&1; }

# The walker resolves its root from where its own script sits, so pointing the shipped copy
# at a scratch tree is not possible. A COPY inside the scratch repository is what lets
# the walk cover that tree rather than the one the shipped copy lives in.
lint_at() {
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/literal-lint.sh" "$@" 2>&1
}

# ---------------------------------------------------------------------------------------
# THE POSITIVE CONTROL — a whole scratch repository, because the walker's job is finding
# the files and no amount of --scan testing exercises that.
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/literal-lint.sh" "$ROOT/spira/literal-lint.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(lint_at "$ROOT")"; rc=$?
is   "an empty index refuses to report clean" "3" "$rc"
want "and says why"                           "refusing to report clean" "$out"

# Plant a literal on a known line: line 1 is the shebang, so the literal is on line 2.
printf '#!/usr/bin/env bash\nSOME="${SPIRA_CI_LABEL:-awaiting-ci}"\n' \
    > "$ROOT/spira/planted.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(lint_at "$ROOT")"; rc=$?
is   "SEEN RED: a configured-name literal is refused" "1" "$rc"
want "and it names the file"                          "spira/planted.sh" "$out"
want "and it names the line"                          "spira/planted.sh:2" "$out"
want "and it names the escape hatch"                  "literal-ok" "$out"

# ---------------------------------------------------------------------------------------
# THE EXEMPTIONS ARE EXACT AND SCOPED. Each carries a configured-name literal legitimately;
# none should appear in the violation output. The plant remains in the tree during this
# test so that a fence that exempts everything would still exit 1 — the only way nowant
# means something here is if the fence is still running and finding the plant.
# ---------------------------------------------------------------------------------------
printf 'schema_name() { printf %%s "${SPIRA_CI_LABEL:-awaiting-ci}"; }\n' > "$ROOT/spira/schema.sh"
printf 'SPIRA_CI_LABEL="${SPIRA_CI_LABEL:-awaiting-ci}"\n'                > "$ROOT/spira/conf.sh"
printf '#!/usr/bin/env bash\nDB_LABEL=awaiting-ci\n'                      > "$ROOT/spira/test-ci.sh"
printf '{"label":"awaiting-ci"}\n'                                         > "$ROOT/spira/labels.json"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add exempted files"

out="$(lint_at "$ROOT")"
want   "plant is still reported while exemptions are checked" "spira/planted.sh" "$out"
nowant "schema.sh is not flagged"                            "schema.sh:"        "$out"
nowant "conf.sh is not flagged"                              "conf.sh:"          "$out"
nowant "test-ci.sh is not flagged"                           "test-ci"           "$out"
nowant "json files are not flagged"                          "labels.json"       "$out"

# A COMPILED BINARY IS NOT SOURCE. bin/queue-watch carried the default label name as a string
# constant and failed PR 331; its source is linted, and a binary has no line to annotate.
mkdir -p "$ROOT/bin"
printf '\177ELF\0\0\0label=awaiting-ci\0\0' > "$ROOT/bin/tool"
git -C "$ROOT" add bin/tool
git -C "$ROOT" commit -q -m "add a binary"
out="$(lint_at "$ROOT")"
want   "plant still reported alongside a binary"                "spira/planted.sh" "$out"
nowant "a compiled binary is not flagged"                       "bin/tool"          "$out"
git -C "$ROOT" rm -q bin/tool
git -C "$ROOT" commit -q -m "remove binary"

# The exemption does not extend to a neighbouring name.
printf '#!/usr/bin/env bash\nDB_LABEL=awaiting-ci\n' > "$ROOT/spira/test-ci-extra.sh"
printf '#!/usr/bin/env bash\nNOT_EXEMPT=awaiting-ci\n' > "$ROOT/spira/uses-literal-2.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add neighbour"
out="$(lint_at "$ROOT")"
want   "a non-exempted file is flagged"            "spira/uses-literal-2.sh" "$out"
nowant "test-ci-extra.sh is not flagged"           "test-ci-extra"           "$out"
git -C "$ROOT" rm -q spira/uses-literal-2.sh spira/test-ci-extra.sh
git -C "$ROOT" commit -q -m "remove"

# Withdrawn, and only now is a green reading evidence of anything.
git -C "$ROOT" rm -q spira/planted.sh
git -C "$ROOT" commit -q -m clean

out="$(lint_at "$ROOT")"; rc=$?
is   "GREEN AFTER: the same tree without the plant passes" "0" "$rc"
want "and reports how many files it checked"               "clean" "$out"

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE. Read through the control above, this now means something.
# ---------------------------------------------------------------------------------------
# In the gate container the worktree's .git FILE contains a gitdir: line that resolves
# to the host — a path that is not bind-mounted inside the container. git exits non-zero
# and literal-lint.sh exits 3 ("not a git repository"). Build a portable mirror from the
# real files, replacing the unreachable worktree gitdir with a plain git repo, and run
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
is   "every shipped file passes" "0" "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

# ---------------------------------------------------------------------------------------
# --scan: matcher, one file at a time; exit 0 either way.
# ---------------------------------------------------------------------------------------
PROBE="$TMP/probe.sh"

printf '#!/usr/bin/env bash\nSOME="${SPIRA_ASK_LABEL:-needs-operator}"\n' > "$PROBE"
out="$(lint --scan "$PROBE")"
want "--scan finds the literal and reports its line" "2:" "$out"
want "and includes the matching text"                "needs-operator" "$out"

# THE ESCAPE HATCH. The same literal is stood down two ways.
printf 'SOME="${SPIRA_ASK_LABEL:-needs-operator}"  # literal-ok: standing it down\n' > "$PROBE"
is   "literal-ok on the offending line silences it"    "" "$(lint --scan "$PROBE")"

printf '# literal-ok: the line below is special\nSOME="${SPIRA_ASK_LABEL:-needs-operator}"\n' > "$PROBE"
is   "literal-ok on the line directly above silences it" "" "$(lint --scan "$PROBE")"

# WHAT IS NOT CODE. A matcher that flags these would be reworded around rather than obeyed.
printf '# default label is needs-operator\n' > "$PROBE"
is   "a pure comment line is not flagged"         "" "$(lint --scan "$PROBE")"

# The same literal without the hatch must be flagged — the pair matters.
printf 'SOME="${SPIRA_ASK_LABEL:-needs-operator}"\n' > "$PROBE"
want "while the same literal without the hatch is caught" "needs-operator" "$(lint --scan "$PROBE")"

# ---------------------------------------------------------------------------------------
# --names: prints every configured name the fence watches for.
# ---------------------------------------------------------------------------------------
out="$(lint --names)"
want "--names includes needs-operator"        "needs-operator"        "$out"
want "--names includes awaiting-ci"           "awaiting-ci"           "$out"
want "--names includes world-stop"            "world-stop"            "$out"
want "--names includes maechen-sweep"         "maechen-sweep"         "$out"
want "--names includes maechen-remedy"        "maechen-remedy"        "$out"
want "--names includes review-finding"        "review-finding"        "$out"
want "--names includes spira-waiting-operator" "spira-waiting-operator" "$out"

# ---------------------------------------------------------------------------------------
# EXPORTED LABELS DON'T MASK DEFAULTS. An aeon's environment may export SPIRA_ASK_LABEL
# to a non-default value; the default must still appear in --names so a literal like
# "${SPIRA_ASK_LABEL:-needs-operator}" is refused in that environment too.
# ---------------------------------------------------------------------------------------
lint_with_labels() {
    SPIRA_ASK_LABEL=needs-ryan SPIRA_CI_LABEL=custom-ci \
        bash "$HERE/literal-lint.sh" "$@" 2>&1
}

out="$(lint_with_labels --names)"
want "SEEN RED: exported SPIRA_ASK_LABEL: defaults still in --names" "needs-operator" "$out"
want "exported SPIRA_ASK_LABEL: configured value also in --names"    "needs-ryan"     "$out"
want "exported SPIRA_CI_LABEL: default still in --names"             "awaiting-ci"    "$out"

# A planted default literal must be refused even when SPIRA_ASK_LABEL is set to something
# else — the gate runs without the aeon's env, so it refuses what the aeon sees as clean.
printf '#!/usr/bin/env bash\nSOME="${SPIRA_ASK_LABEL:-needs-operator}"\n' > "$PROBE"
out="$(lint_with_labels --scan "$PROBE")"
want "SEEN RED: default literal refused with exported SPIRA_ASK_LABEL" "needs-operator" "$out"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION. A fence nothing invokes is a file; this is the one property no amount
# of matcher testing can establish.
# ---------------------------------------------------------------------------------------
want "the gate names this fence" "spira/literal-lint.sh" "$(cat "$HERE/gate-spira.sh")"
is   "and it is executable"      "0" "$([ -x "$HERE/literal-lint.sh" ]; echo $?)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
