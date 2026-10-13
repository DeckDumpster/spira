#!/usr/bin/env bash
#
# test-incident-cause-rule.sh — every SPIRA_INCIDENT_REF filing site declares SPIRA_INCIDENT_CAUSE.
#
# tier: T0
# covers: spira-lint/src/rules/incident_cause_lint.rs spira/*.sh UC-ops-detection-remediation-09
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

echo "test-incident-cause-rule.sh"
command -v spira-lint >/dev/null 2>&1 || bail "spira-lint is not on PATH"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
git init -q "$TMP/t" && mkdir -p "$TMP/t/spira"
lint() { spira-lint --root "$1" --only incident-cause-lint 2>&1; }

printf '#!/usr/bin/env bash\nSPIRA_INCIDENT_REF=incident:planted bash incident.sh\n' >"$TMP/t/spira/offender.sh"
git -C "$TMP/t" add -A
out="$(lint "$TMP/t")"; rc=$?
is   "a planted ref with no cause is refused" 1 "$rc"
want "the finding names the offender line" "spira/offender.sh:2" "$out"

printf 'SPIRA_INCIDENT_CAUSE=planted\n' >>"$TMP/t/spira/offender.sh"
git -C "$TMP/t" add -A
out="$(lint "$TMP/t")"; rc=$?
is "a declared cause beside the ref is clean" 0 "$rc"

# The rule scans spira/*.sh only. Lint a fresh repository holding exactly those files rather
# than $ROOT itself: in testenv the checkout is a git worktree bind-mounted alone, its `.git`
# file names a host gitdir the container cannot see, so `git ls-files` fails, spira-lint exits
# 4 ("not a git repository") and this case was red on every host gate (79 of 79) while green on
# every round VM (a full clone). .gitignore comes along so ignored generated files stay ignored.
mkdir -p "$TMP/tree" && git init -q "$TMP/tree"
cp "$ROOT/.gitignore" "$TMP/tree/" 2>/dev/null
(cd "$ROOT" && find spira -name '*.sh' -type f -print0 | tar --null -T - -cf -) | tar -xf - -C "$TMP/tree" \
    || bail "could not copy spira/*.sh from $ROOT"
n="$(find "$TMP/tree/spira" -name '*.sh' -type f | wc -l)"
[ "$n" -gt 100 ] || bail "copied only $n spira/*.sh files from $ROOT — the copy, not the tree, is broken"
git -C "$TMP/tree" add -A
out="$(lint "$TMP/tree")"; rc=$?
is "the tree declares a cause at every ref site" 0 "$rc"
[ "$rc" = 0 ] || printf '%s\n' "$out"

tl_summary
