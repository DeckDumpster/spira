#!/usr/bin/env bash
#
# test-certify-critical-covers.sh — SPIRA_GATE_SUITES=off still runs the suites that declare
#   `# covers:` on a file named in SPIRA_CERTIFY_ALWAYS_COVERS (default spira/lib.sh).
#
# THE CASE (sp-k7sz0). Certification runs with SPIRA_GATE_SUITES=off (SPIRA_CERTIFY_SUITES=off
# — see test-certify-suites-off.sh), which selects nothing at all so a closed bead's own gate
# does not time out running the full corpus. sp-dgaig's rewrite of landed() in spira/lib.sh
# was certified under that switch, selected no suites, and reached batch 323 unverified —
# where it broke two suites that both declare `# covers: spira/lib.sh`. A shared library is
# the highest-fanout file in the tree; the off switch must not blind certification to it.
#
# PROPERTIES (law-absence-needs-a-positive-control):
#   1. off + a critical-file change: the covering suite is selected.
#   2. off + a critical-file change: a suite covering something else is NOT selected.
#   3. off + a critical-file change: a suite with no # covers: line (normally always-run) is
#      NOT pulled in — the carve-out stays targeted, not a reopening of the full off switch.
#   4. off + an UNRELATED change: nothing is selected (POSITIVE CONTROL for 1 — proves the
#      selection above came from the critical-file match, not from suites=off doing nothing
#      at all).
#   5. on (suites not off): the same critical-file change selects the covering suite AND the
#      no-covers suite — proves case 3's exclusion is specific to the off-mode carve-out, not
#      a broken suite.
#   6. SPIRA_CERTIFY_ALWAYS_COVERS is configurable: overriding it to a glob that does not
#      match the changed file drops the selection back to nothing.
#
# tier: T1
# covers: spira/gate-touched.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TOUCHED="$HERE/gate-touched.sh"

# ---------------------------------------------------------------------------
# FIXTURE
# spira/lib.sh:     the critical file (default SPIRA_CERTIFY_ALWAYS_COVERS).
# spira/other.sh:   an unrelated file.
# test-crit.sh:     covers spira/lib.sh — should survive the off-mode carve-out.
# test-other.sh:    covers spira/other.sh — should not.
# test-nocov.sh:    no covers line (always-run under suites=on) — should not survive the
#                   carve-out either; the carve-out only runs suites an explicit covers:
#                   line named.
# ---------------------------------------------------------------------------
R="$TMP/repo"; git init -q -b main "$R"; mkdir -p "$R/spira"
printf 'x\n' > "$R/spira/lib.sh"
printf 'x\n' > "$R/spira/other.sh"
printf '#!/usr/bin/env bash\n# covers: spira/lib.sh\nexit 0\n'   > "$R/spira/test-crit.sh"
printf '#!/usr/bin/env bash\n# covers: spira/other.sh\nexit 0\n' > "$R/spira/test-other.sh"
printf '#!/usr/bin/env bash\nexit 0\n'                            > "$R/spira/test-nocov.sh"
git -C "$R" add -A; git -C "$R" commit -q -m base

git -C "$R" checkout -q -b br-lib
printf 'y\n' >> "$R/spira/lib.sh"
git -C "$R" add -A; git -C "$R" commit -q -m "change lib.sh"

git -C "$R" checkout -q main -b br-other
printf 'y\n' >> "$R/spira/other.sh"
git -C "$R" add -A; git -C "$R" commit -q -m "change other.sh"

sel() {  # sel <branch> [SPIRA_GATE_SUITES value] [SPIRA_CERTIFY_ALWAYS_COVERS value]
    (cd "$R" && SPIRA_GATE_REPO="$R" SPIRA_BATCH_SUITE_DIR="$R/spira" \
        SPIRA_GATE_SUITES="${2:-off}" SPIRA_CERTIFY_ALWAYS_COVERS="${3:-}" \
        bash "$TOUCHED" main "$1" 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')
}

echo "test-certify-critical-covers.sh"
echo

echo "1-3. off + critical-file change:"
sel_lib="$(sel br-lib)"
want   "1: covering suite selected"          "test-crit.sh"  "$sel_lib"
nowant "2: unrelated-covering suite excluded" "test-other.sh" "$sel_lib"
nowant "3: no-covers suite excluded (carve-out stays targeted)" "test-nocov.sh" "$sel_lib"

echo
echo "4. off + unrelated change (POSITIVE CONTROL for 1):"
is "4: nothing selected" "" "$(sel br-other)"

echo
echo "5. on (suites not off): no-covers suite comes back (positive control for 3)"
sel_on="$(sel br-lib on)"
want "5a: covering suite still selected"  "test-crit.sh"  "$sel_on"
want "5b: no-covers suite selected when on" "test-nocov.sh" "$sel_on"

echo
echo "6. SPIRA_CERTIFY_ALWAYS_COVERS is configurable:"
is "6: overriding the critical glob away from lib.sh drops the selection" "" \
    "$(sel br-lib off 'spira/nonexistent.sh')"

echo
tl_summary
