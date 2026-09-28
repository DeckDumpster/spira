#!/usr/bin/env bash
#
# moot-sweep.sh — resolve asks whose MOOT-WHEN predicate now exits 0.
#
#   moot-sweep.sh [--apply]
#
# An auto-filed ask can record the condition that fired it as a MOOT-WHEN: line in its
# description:
#
#     MOOT-WHEN: <shell command exiting 0 when the condition has cleared>
#
# When that command exits 0 the alert condition is gone and the ask is no longer actionable.
# Without --apply this reports which asks would be resolved; with --apply it resolves them.
#
# THREE INVARIANTS
#
#   An ask with no MOOT-WHEN is NEVER touched. Hand-written escalations and policy questions
#   carry no mechanical predicate; auto-closing one would destroy an escalation the operator
#   never saw.
#
#   A predicate that errors or times out leaves the ask open. A failed probe must never read
#   as "condition cleared" — that is the ? not 0 rule (law-absence-needs-a-positive-control).
#
#   Resolution goes through resolve.sh, NEVER a raw bd close. resolve.sh records the actor
#   as claude, so the audit trail can always tell this close from the operator's own.
set -uo pipefail

. "$(dirname "$0")/db.sh"

apply=""; [ "${1:-}" = "--apply" ] && apply=1

db=$(cockpit_db) || {
    echo "  moot-sweep: no beads database — checked nothing"
    exit 0
}
rows=$(cockpit_attention_beads) || {
    echo "  moot-sweep: the beads database is not reachable — checked nothing"
    exit 0
}

RESOLVE_SH="${SPIRA_RESOLVE_SH:-$(dirname "$0")/resolve.sh}"

found=0; resolved=0; errors=0
while IFS=$'\t' read -r id pred; do
    [ -n "$id" ] || continue
    found=$((found+1))
    out=$(timeout "${SPIRA_MOOT_TIMEOUT:-30}" bash -c "$pred" 2>&1); rc=$?
    short=$(printf '%s' "$out" | head -c 300)
    if [ "$rc" -eq 0 ]; then
        echo "  CLEARED    $id  ($pred)"
        if [ -n "$apply" ]; then
            reason="auto-resolved: MOOT-WHEN predicate cleared — ${pred} → exit 0. Evidence: ${short}"
            if bash "$RESOLVE_SH" "$id" "$reason" >/dev/null 2>&1; then
                echo "    resolved"
                resolved=$((resolved+1))
            else
                echo "    resolve FAILED"
                errors=$((errors+1))
            fi
        fi
    elif [ "$rc" -eq 124 ]; then
        errors=$((errors+1))
        echo "  TIMEOUT    $id  (timed out after ${SPIRA_MOOT_TIMEOUT:-30}s — predicate: ${pred})"
    else
        echo "  still live $id  (predicate exit $rc)"
    fi
done < <(python3 -c '
import json, os, re, sys
ASK = os.environ.get("SPIRA_ASK_LABEL", "needs-operator")  # literal-ok: Python fallback for direct invocation without conf.sh
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(0)
rows = doc if isinstance(doc, list) else doc.get("issues", [])
for r in rows:
    labels = r.get("labels") or []
    if ASK not in labels:
        continue
    if (r.get("status") or "") != "open":
        continue
    m = re.search(r"^\s*MOOT-WHEN:\s*(.+)$", r.get("description") or "", re.M)
    if m:
        print("%s\t%s" % (r.get("id"), m.group(1).strip()))
' <<<"$rows")

echo "  ${found} ask(s) carry a predicate; ${resolved} resolved${errors:+; ${errors} error(s)}"
exit 0
