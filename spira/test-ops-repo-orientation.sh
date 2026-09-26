#!/usr/bin/env bash
#
# test-ops-repo-orientation.sh — the Ops persona must say where the worktree is before it
# shows a single production-checkout path.
#
#   ./test-ops-repo-orientation.sh
#
# THE DEFECT (sp-0wow1). ops.md's very first instructions are commands built from
# {{INCIDENT}}, {{SOP}}, {{ASK}} and {{SUITES}}, which all render to $SPIRA_HOME — the
# production checkout, correct for those read-only utilities. The persona did not say
# {{REPO}} (the aeon's own worktree, where code is Read and Edited) was a *different* tree
# until "How you must work", well past a dozen production-path examples. An aeon that
# reads a source file to diagnose an incident carried the only path prefix it had seen —
# $SPIRA_HOME — into the Read, then into the Edit, writing its fix into production instead
# of its worktree (run/sp-5l3zv.log: the aeon Read, then Edited,
# $SPIRA_HOME/spira/mail.sh, and only much later discovered {{REPO}} was different).
#
# THE FIX. A "Where you are" section, naming {{REPO}} as the only place to Read or Edit
# code and {{SPIRA_HOME}} as invoke-only, now opens ops.md before "The loop" ever shows a
# {{SPIRA_HOME}}-rooted command.
#
# This is a static text-order check, not a runtime guard: it reads the chamber persona
# template, not a live session (law-a-matcher-reads-code-not-prose still applies — the
# check is on the file the model is actually handed, not a paraphrase of it).
#
# POSITIVE CONTROL FIRST. A corrupted copy with the orientation moved below the first
# production-path example must fail (SEEN RED) before the real file's order is trusted.
#
# tier: T0
# covers: spira/chamber/ops.md
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"
OPS_MD="$HERE/chamber/ops.md"

[ -f "$OPS_MD" ] || { printf 'FATAL: ops.md not found at %s\n' "$OPS_MD" >&2; exit 1; }

# check <label> <file> — line of first {{REPO}} vs. first {{SPIRA_HOME}}-family
# placeholder. Exits 0 (repo orientation comes first, or no tool path exists to race it),
# 1 (a tool path appears with no orientation before it — the defect), 3 (neither marker
# found in a non-empty file — can't examine, law-absence-needs-a-positive-control).
check() {
    local label="$1" file="$2" repo_line home_line
    [ -s "$file" ] || { printf '%s: empty or missing file — cannot examine\n' "$label" >&2; return 3; }
    repo_line="$(grep -n '{{REPO}}' "$file" | head -1 | cut -d: -f1)"
    home_line="$(grep -nE '\{\{(SPIRA_HOME|INCIDENT|SOP|ASK|SUITES)\}\}' "$file" | head -1 | cut -d: -f1)"
    if [ -z "${home_line:-}" ]; then
        printf '%s: no production-path placeholder present — nothing to race\n' "$label"
        return 0
    fi
    if [ -z "${repo_line:-}" ]; then
        printf '%s: a production path appears at line %s with no {{REPO}} orientation anywhere\n' \
            "$label" "$home_line" >&2
        return 1
    fi
    if [ "$repo_line" -le "$home_line" ]; then
        printf '%s: {{REPO}} orientation (line %s) precedes the first production path (line %s)\n' \
            "$label" "$repo_line" "$home_line"
        return 0
    fi
    printf '%s: production path at line %s appears before {{REPO}} orientation at line %s\n' \
        "$label" "$home_line" "$repo_line" >&2
    return 1
}

echo "test-ops-repo-orientation.sh"

# =======================================================================================
echo
echo "POSITIVE CONTROL: an ops.md with the orientation moved below the first production"
echo "path must be refused:"
echo "-----------------------------------------------------------------------"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
BAD="$TMP/ops-corrupt.md"

# Reconstruct the pre-fix shape: strip the "## Where you are" section (from that heading
# up to, but not including, the next "## " heading) and drop it in after "## The loop"
# instead — after {{INCIDENT}} has already appeared.
python3 - "$OPS_MD" "$BAD" <<'PY'
import re, sys
src, dst = sys.argv[1], sys.argv[2]
text = open(src).read()
m = re.search(r'\n## Where you are\n.*?(?=\n## )', text, re.S)
assert m, "fixture setup: ops.md has no '## Where you are' section to relocate"
section = m.group(0)
without = text[:m.start()] + text[m.end():]
# Reinsert it right after the first placeholder-bearing line, well past {{INCIDENT}}.
idx = without.index('{{INCIDENT}} list')
line_end = without.index('\n', idx)
corrupted = without[:line_end+1] + section + without[line_end+1:]
open(dst, 'w').write(corrupted)
PY

if check "positive control" "$BAD"; then
    bad "positive control: corrupted ops.md is refused" "check exited 0 (should have failed)"
else
    ok "positive control: corrupted ops.md is refused"
fi

# =======================================================================================
echo
echo "SEEN GREEN: the real ops.md orders {{REPO}} before any production path:"
echo "-----------------------------------------------------------------------"
if check "ops.md" "$OPS_MD"; then
    ok "ops.md: {{REPO}} orientation precedes every production-path placeholder"
else
    bad "ops.md: {{REPO}} orientation precedes every production-path placeholder" \
        "check exited non-zero — see stderr above"
fi

echo
tl_summary
