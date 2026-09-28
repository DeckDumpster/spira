#!/usr/bin/env bash
#
# test-binary-path-fence.sh — the T0 fence that refuses a hardcoded binary path outside
# conf.sh's spira_bin.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A fence that reports a clean tree is
# indistinguishable from one whose matcher never fires, an empty glob, or a bad root — and
# the reassuring reading is what all three give. A literal is planted in a scratch
# repository, the fence is required to name its file and line, the plant is withdrawn, and
# only then is the shipped tree's silence evidence of anything
# (law-absence-needs-a-positive-control).
#
# tier: T0
# covers: spira/binary-path-fence.sh spira/gate-fences.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-binary-path-fence.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

fence_at() {
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/binary-path-fence.sh" "$@" 2>&1
}

# ---------------------------------------------------------------------------------------
# THE WALK — a scratch repository, because the walker's job is finding the files and no
# amount of --scan testing exercises that.
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/binary-path-fence.sh" "$ROOT/spira/binary-path-fence.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(fence_at "$ROOT")"; rc=$?
is   "an empty index refuses to report clean" "3" "$rc"
want "and says why"                            "refusing to report clean" "$out"

# Plant an offender on a known line: line 1 is the shebang, so the literal is on line 2.
printf '#!/usr/bin/env bash\nBIN="$SPIRA_REPO/target/release/reconciler"\n' \
    > "$ROOT/spira/planted.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: a hardcoded target/release path is refused" "1" "$rc"
want "and it names the file"                                 "spira/planted.sh" "$out"
want "and it names the line"                                 "spira/planted.sh:2" "$out"
want "and it names the escape hatch"                          "path-ok" "$out"

# ---------------------------------------------------------------------------------------
# bin/spira-<name> is refused the same as target/release/ — both are a second resolver.
# ---------------------------------------------------------------------------------------
printf '#!/usr/bin/env bash\nBIN="$SPIRA_REPO/bin/spira-supervise"\n' > "$ROOT/spira/planted2.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add second plant"
out="$(fence_at "$ROOT")"
want "bin/spira-<name> is refused too" "spira/planted2.sh:2" "$out"
git -C "$ROOT" rm -q spira/planted2.sh
git -C "$ROOT" commit -q -m "remove second plant"

# ---------------------------------------------------------------------------------------
# THE ESCAPES ARE EXACT AND SCOPED: the marker line, the allow file, and doc/fixture data.
# The first plant remains in the tree during this section so a fence that exempts
# everything would still exit 1 — the only way nowant means something here is if the fence
# is still running and finding the plant.
# ---------------------------------------------------------------------------------------
printf '#!/usr/bin/env bash\nBIN="$SPIRA_REPO/target/release/marked" # path-ok: test fixture\n' \
    > "$ROOT/spira/marked.sh"
printf 'spira/allowed.sh\n' > "$ROOT/spira/binary-path-fence-allow"
printf '#!/usr/bin/env bash\nBIN="$SPIRA_REPO/target/release/allowed"\n' > "$ROOT/spira/allowed.sh"
printf '# see target/release/doc-only\n' > "$ROOT/docs.md"
printf '{"note":"target/release/fixture"}\n' > "$ROOT/data.json"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "add exemptions"

out="$(fence_at "$ROOT")"
want   "plant is still reported while exemptions are checked" "spira/planted.sh" "$out"
nowant "a marked line is not flagged"                          "marked.sh:"       "$out"
nowant "an allow-listed file is not flagged"                   "allowed.sh:"      "$out"
nowant "a doc file is not flagged"                             "docs.md"          "$out"
nowant "a json fixture is not flagged"                         "data.json"        "$out"

# Withdrawn, and only now is a green reading evidence of anything.
git -C "$ROOT" rm -q spira/planted.sh
git -C "$ROOT" commit -q -m clean

out="$(fence_at "$ROOT")"; rc=$?
is   "clean tree: exit 0"        "0"     "$rc"
want "and says clean"            "clean" "$out"

tl_summary
