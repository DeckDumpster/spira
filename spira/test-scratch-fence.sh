#!/usr/bin/env bash
#
# test-scratch-fence.sh — positive control for the scratch-file fence.
#
#   ./test-scratch-fence.sh
#
# WHAT THIS SUITE IS FOR
# ----------------------
# scratch-fence.sh refuses tracked files matching sp-* or *.fixed at the repository
# root. These are aeon working notes; they reach every consumer of this repository and
# trigger the all-suites fallback in coverage selection, so one note costs ~65 minutes
# of suite time on every branch that follows it.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A fence that reports a clean tree is
# indistinguishable from one whose matcher never fires — an empty root, a bad path, and
# a silent grep all give the same silence. An offender is planted in a scratch
# repository, the fence is required to name it (SEEN RED), the plant is withdrawn, and
# only then is the fence's silence evidence of a clean tree
# (law-absence-needs-a-positive-control).
#
# host-reason: tests the scratch-fence using scratch git repos; no container-hosted state
#
# tier: T1
# covers: spira/scratch-fence.sh spira/gate-spira.sh UC-safety-fences-24
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
is()     { [ "$2" = "$3" ] && ok "$1" || bad "$1" "wanted [$2] got [$3]"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-scratch-fence.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# The fence self-locates its root from its own script path, so the test copies it to a
# scratch repository and runs it from there — the only way to target the walk at the
# scratch tree rather than the shipped one.
fence_at() {
    local root="$1"; shift
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/scratch-fence.sh" 2>&1
}

# ---------------------------------------------------------------------------------------
# EMPTY INDEX — must exit 3, refusing to report clean (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/scratch-fence.sh" "$ROOT/spira/scratch-fence.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t

out="$(fence_at "$ROOT")"; rc=$?
is   "empty index exits 3 (refuses to report clean)" "3" "$rc"
want "and says why"                                   "refusing to report clean" "$out"

# Seed the tree with one legitimate file so the positive-control check passes.
printf '#!/usr/bin/env bash\necho ok\n' > "$ROOT/spira/helper.sh"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m init

out="$(fence_at "$ROOT")"; rc=$?
is   "clean tree exits 0" "0" "$rc"
want "and says so"        "no aeon scratch files" "$out"

# ---------------------------------------------------------------------------------------
# SEEN RED: sp-* file planted at the root.
# ---------------------------------------------------------------------------------------
printf 'investigation notes\n' > "$ROOT/sp-xxxx-notes.md"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "plant sp-* scratch file"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: sp-* at root is refused" "1" "$rc"
want "names the file"                    "sp-xxxx-notes.md" "$out"
want "mentions the override"             "SCRATCH_FENCE_OK" "$out"

# ---------------------------------------------------------------------------------------
# SEEN GREEN: withdraw the sp-* offender.
# ---------------------------------------------------------------------------------------
git -C "$ROOT" rm -qf sp-xxxx-notes.md
git -C "$ROOT" commit -q -m "withdraw sp-* scratch file"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean after sp-* withdrawal" "0" "$rc"
want "and says so"                             "no aeon scratch files" "$out"

# ---------------------------------------------------------------------------------------
# SEEN RED: *.fixed file planted at the root.
# ---------------------------------------------------------------------------------------
printf '#!/usr/bin/env bash\necho patched\n' > "$ROOT/some-script.sh.fixed"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "plant .fixed artefact"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: *.fixed at root is refused" "1" "$rc"
want "names the file"                        "some-script.sh.fixed" "$out"

# ---------------------------------------------------------------------------------------
# OVERRIDE: SCRATCH_FENCE_OK=1 exits 0 despite offenders.
# ---------------------------------------------------------------------------------------
out="$(env -i PATH="$PATH" HOME="$TMP" TERM=dumb SCRATCH_FENCE_OK=1 \
    bash "$ROOT/spira/scratch-fence.sh" 2>&1)"; rc=$?
is "override exits 0" "0" "$rc"

# ---------------------------------------------------------------------------------------
# SEEN GREEN: withdraw the *.fixed offender.
# ---------------------------------------------------------------------------------------
git -C "$ROOT" rm -qf some-script.sh.fixed
git -C "$ROOT" commit -q -m "withdraw .fixed artefact"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean after .fixed withdrawal" "0" "$rc"

# ---------------------------------------------------------------------------------------
# SUBDIRECTORY FILES ARE NOT FLAGGED. A sp-* file inside spira/ is a legitimate
# suite name; only the root is the forbidden zone.
# ---------------------------------------------------------------------------------------
printf 'notes\n' > "$ROOT/spira/sp-abc-notes.md"
git -C "$ROOT" add .
git -C "$ROOT" commit -q -m "plant sp-* in subdirectory"

out="$(fence_at "$ROOT")"; rc=$?
is   "sp-* in subdirectory does not trigger" "0" "$rc"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION (D7): whether the gate is wired to scratch-fence.sh is
# test-gate-fences.sh's row, reading gate_fence_list rather than grepping this file's
# source for the name. PRE-COMMIT INTEGRATION: whether the hook calls scratch-fence.sh is
# UC-safety-fences-22's commit-through-hook row (sp-pohf2) — a real commit through the
# armed hook proves the wiring; a grep of the hook's source only proved the string was
# still there.
# ---------------------------------------------------------------------------------------

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
