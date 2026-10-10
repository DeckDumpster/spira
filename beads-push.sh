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

trap 'rc=$?; [ "$rc" -eq 0 ] || echo "beads-push: $(date -u +%Y-%m-%dT%H:%M:%SZ) exited $rc at line ${LINENO}" >&2' EXIT

. "$(cd "$(dirname "${BASH_SOURCE[0]}")/spira" && pwd -P)/conf.sh"
export BEADS_NO_AUTO_IMPORT=1
spira_require bd || { echo "beads-push: required dependency check failed (bd)" >&2; exit 1; }

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

fail_out() { echo "beads-push: $stamp — $1" >&2; exit 1; }

# ── PRE-PUSH: commit any dirty tracked tables ─────────────────────────────────
# bd never creates a Dolt commit for config writes (statutes, memories), so a bare
# push leaves those changes behind and a fresh clone is short however many statutes
# were enacted since the last issue write.
#
# WHY A RUST BINARY, NEVER bd. `bd dolt commit` returns 0 and "Nothing to commit." for a
# working set the real dolt engine can see. beads-store resolves the store's real engine
# (a data directory, or the sql-server named in .beads/metadata.json) and commits and
# verifies through that alone (beads-store/DESIGN.md).
command -v beads-store >/dev/null 2>&1 \
    || fail_out "beads-store not on PATH — cannot commit before push."
commit_out="$(beads-store commit --db "$DB" --message "beads-push: $stamp" 2>&1)"
commit_rc=$?
[ "$commit_rc" -eq 0 ] || fail_out "$commit_out"
if [ "$commit_out" -gt 0 ] 2>/dev/null; then
    echo "beads-push: $stamp — committed ${commit_out} dirty table(s) ($(_bp_statute_count "$DB") statutes)"
fi

# ── PUSH, THEN VERIFY IT BY ITS EFFECT ────────────────────────────────────────
# "Push complete" is a claim about a command, not about the remote. beads-store pushes,
# refreshes the tracking ref and compares both heads through the one engine it resolved
# in-process (never an env var or a second engine), so the store verified is the store
# pushed, and only that comparison produces the OK line.
# A failed push is the expensive one: it packs the whole repository before the remote refuses
# or the deadline kills it. So a failure parks further attempts for a day instead of
# repacking on every timer tick.
failed_stamp="${SPIRA_RUN:-/tmp}/beads-push.failed-at"
retry_after="${BEADS_PUSH_RETRY_AFTER_FAILURE_SECS:-86400}"
if [ -f "$failed_stamp" ]; then
    since=$(( $(date +%s) - $(cat "$failed_stamp" 2>/dev/null || echo 0) ))
    if [ "$since" -lt "$retry_after" ]; then
        fail_out "last push failed ${since}s ago; not repacking again for $((retry_after - since))s (remove $failed_stamp to retry now)"
    fi
fi

deadline="${BEADS_PUSH_DEADLINE_SECS:-600}"
remote_head="$(BEADS_STORE_PUSH_DEADLINE_SECS="$deadline" ionice -c3 nice -n 19 \
    beads-store push --db "$DB" --remote beads 2>&1)" || {
    date +%s > "$failed_stamp"
    fail_out "${remote_head:-push exited non-zero with no output}"
}
rm -f "$failed_stamp"

echo "beads-push: $stamp — spira OK ($(_bp_statute_count "$DB") statutes, remote at $remote_head)"
exit 0
