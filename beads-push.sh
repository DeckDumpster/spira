#!/usr/bin/env bash
# beads-push.sh — push Spira's beads database to its configured Dolt remote.
#
# WHY THIS EXISTS. A one-shot setup script wired real, private GitHub remotes onto several
# beads databases and nothing ever pushed to them again — measured 2026-09-04, three of them
# had last received data on 2026-09-02 within 36 seconds of each other, which is the
# signature of a setup script and not of a backup. A configured remote that nothing pushes
# is worse than no remote, because it looks like a backup on inspection and answers "is this
# backed up?" with a yes it has not earned.
#
# WHY SPIRA ALONE. Spira is the only beads database anything still writes. The predecessor
# harness's stores are frozen, and pushing a store nothing writes is churn that looks like a
# live backup while carrying no new data; their final contents already reached their remotes.
#
# The JSONL export is the other half of a backup and covers different ground: it is diffable
# and readable without any tooling, but carries only the issues table and the memories.
# This job carries the Dolt store itself — branches, history, working set. Keep whatever
# repository either lands in PRIVATE; a beads database is never public.
#
# WHAT IT DOES NOT DO. It never creates a repository and never wires a remote. Opting a
# database in is a deliberate act, so a database with no remote is reported and skipped
# rather than treated as a failure — a clean clone has none, and a timer that fails every
# six hours on a box that was never opted in is a false alert.
set -uo pipefail

. "$(cd "$(dirname "${BASH_SOURCE[0]}")/spira" && pwd -P)/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
spira_require bd || exit 1

DB="$SPIRA_DB"
stamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [ ! -d "$DB/.beads" ]; then
    echo "beads-push: $stamp — no beads database at $DB" >&2
    exit 1
fi

# --remote beads is the name make-beads-repo.sh wires alongside origin.
if ! grep -q '^sync.remote:' "$DB/.beads/config.yaml" 2>/dev/null; then
    echo "beads-push: $stamp — no Dolt remote configured; nothing to push"
    exit 0
fi

# ── PRE-PUSH: commit any dirty tracked tables ─────────────────────────────────
# bd never creates a Dolt commit for config writes (statutes, memories), so a bare
# push leaves those changes behind and a fresh clone is short however many statutes
# were enacted since the last issue write. An empty working set is a no-op —
# bd dolt commit refuses to create an empty commit.
#
# TWO QUERY PATHS: bd sql (server mode) and dolt --data-dir (embedded mode).
# The embedded binary refuses bd sql; dolt can read the same storage directly.
_bp_dirty_count() {
    local db="$1" n=""
    # Server mode.
    n=$(bd -C "$db" sql "SELECT COUNT(*) FROM dolt_status" 2>/dev/null \
        | sed -n '3p' | tr -d ' ')
    [[ "${n:-}" =~ ^[0-9]+$ ]] && { printf '%s' "$n"; return; }
    # Embedded mode.
    local doltdb="$db/.beads/embeddeddolt"
    [ -d "$doltdb" ] && command -v dolt >/dev/null 2>&1 || { printf '0'; return; }
    local dbn
    dbn="$(ls "$doltdb" 2>/dev/null | grep -v '^\.' | grep -v '^\.lock$' | head -1)" \
        || dbn="sp"
    n="$(dolt --data-dir "$doltdb" sql -q \
        "use ${dbn:-sp}; SELECT COUNT(*) FROM dolt_status;" \
        2>/dev/null | sed -n '4p' | tr -d '| ')"
    [[ "${n:-}" =~ ^[0-9]+$ ]] && printf '%s' "$n" || printf '0'
}

_bp_statute_count() {
    bd -C "$1" memories --json 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    print(sum(1 for k in d if k.startswith("law-")))
except Exception:
    print("?")
' 2>/dev/null || echo "?"
}

dirty="$(_bp_dirty_count "$DB")"
if [ "${dirty:-0}" -gt 0 ]; then
    commit_out=$(bd -C "$DB" dolt commit -m "beads-push: $stamp" 2>&1)
    commit_rc=$?
    if [ "$commit_rc" -ne 0 ]; then
        echo "beads-push: $stamp — pre-push commit failed: $(tail -2 <<<"$commit_out" | tr '\n' ' ')" >&2
        exit 1
    fi
    echo "beads-push: $stamp — committed ${dirty} dirty table(s) ($(_bp_statute_count "$DB") statutes)"
fi

out=$(timeout 900 bd -C "$DB" dolt push --remote beads 2>&1)
if grep -q 'Push complete' <<<"$out"; then
    echo "beads-push: $stamp — spira OK ($(_bp_statute_count "$DB") statutes)"
    exit 0
fi

# Loud, and with the reason attached: a silent push failure is the exact shape of the
# problem this script was written to fix.
echo "beads-push: $stamp — spira FAILED — $(tail -2 <<<"$out" | tr '\n' ' ')" >&2
exit 1
