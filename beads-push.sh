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

# ── WHICH ENGINE TELLS THE TRUTH ──────────────────────────────────────────────
# Two engines can read this store and they do not always agree. On 2026-09-15 the
# config table held 144 uncommitted rows — the whole statute book — and `bd sql
# "SELECT COUNT(*) FROM dolt_status"` answered 0 while the dolt CLI reading the same
# database answered 1. bd's own `dolt commit` said "nothing to commit" for the same
# working set. The CLI was right: committing through it cleared the rows and the push
# that followed moved the remote four and a half hours forward.
#
# So the CLI is preferred wherever a data directory is configured, and whichever engine
# answers the dirty probe is the engine that must also commit and verify. Mixing them is
# how the working set one engine cannot see gets left behind by the other.
_BP_ENGINE=""          # "dolt" or "bd" — set by _bp_probe_engine
_BP_DATADIR=""         # dolt --data-dir for the "dolt" engine
_BP_DBNAME=""

# WHERE THE DATA ACTUALLY IS. Three shapes exist and they are not interchangeable:
#
#   server-mode   the store is a sql-server; SPIRA_DOLT_DATA names its data dir and
#                 .beads/dolt is an EMPTY directory. Probing .beads/dolt finds nothing
#                 and, in the old code, printed 0 — a live store reporting itself clean.
#   embedded      the database lives under $DB/.beads/embeddeddolt/<name>.
#   neither       no data dir is discoverable; only bd can speak for the store.
_bp_resolve_store() {
    local db="$1" dd="${SPIRA_DOLT_DATA:-}" emb="$1/.beads/embeddeddolt" n

    command -v dolt >/dev/null 2>&1 || { _BP_ENGINE="bd"; return 0; }

    if [ -n "$dd" ] && [ -d "$dd" ]; then
        _BP_ENGINE="dolt"; _BP_DATADIR="$dd"
        _BP_DBNAME="$(python3 -c '
import json,sys
try: print(json.load(open(sys.argv[1])).get("dolt_database") or "spira")
except Exception: print("spira")
' "$db/.beads/metadata.json" 2>/dev/null)" || _BP_DBNAME="spira"
        [ -n "$_BP_DBNAME" ] || _BP_DBNAME="spira"
        return 0
    fi

    if [ -d "$emb" ]; then
        n="$(ls "$emb" 2>/dev/null | grep -v '^\.' | head -1)"
        if [ -n "$n" ]; then
            _BP_ENGINE="dolt"; _BP_DATADIR="$emb"; _BP_DBNAME="$n"; return 0
        fi
    fi

    _BP_ENGINE="bd"
    return 0
}

_bp_dbname() { printf '%s' "$_BP_DBNAME"; }

# _bp_sql <db> <query> -> rows on stdout, non-zero if the engine could not answer.
_bp_sql() {
    local db="$1" q="$2"
    if [ "$_BP_ENGINE" = "dolt" ]; then
        dolt --data-dir "$_BP_DATADIR" --use-db "$_BP_DBNAME" sql -q "$q" -r csv 2>/dev/null
    else
        bd -C "$db" sql "$q" 2>/dev/null
    fi
}

# _bp_number <db> <query> -> the single numeric cell, or non-zero if it is not a number.
# EVERY caller must check the status. A probe that cannot answer must never be allowed
# to look like a zero (law-absence-needs-a-positive-control) — that is the exact defect
# this file shipped: an unreachable store reported itself clean and the job exited 0.
_bp_number() {
    local out n
    out="$(_bp_sql "$1" "$2")" || return 1
    n="$(printf '%s\n' "$out" | tr -d ' |\r' | grep -E '^[0-9]+$' | tail -1)"
    [ -n "$n" ] || return 1
    printf '%s' "$n"
}

# Resolve the store, then REQUIRE the chosen engine to answer. There is deliberately no
# fallback from a resolved dolt store to bd: on this store bd answered 0 for a working set
# holding 144 uncommitted rows, so quietly asking it instead would rebuild the original
# defect with an extra step. A silent downgrade to a less trustworthy answer is the thing
# this file exists to prevent.
_bp_probe_engine() {
    local db="$1"
    _bp_resolve_store "$db"
    _bp_number "$db" "select count(*) as n from dolt_status;" >/dev/null 2>&1 && return 0
    _BP_ENGINE=""
    return 1
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

_bp_dirty_count() { _bp_number "$1" "select count(*) as n from dolt_status;"; }

_bp_head() {   # _bp_head <db> <revision>
    local out
    out="$(_bp_sql "$1" "select commit_hash from dolt_log('$2') limit 1;")" || return 1
    printf '%s\n' "$out" | tr -d ' |\r' \
        | grep -Ev '^(commit_hash|-+|\(.*rows?\)|)$' | head -1
}

fail_out() { echo "beads-push: $stamp — $1" >&2; exit 1; }

# ── PRE-PUSH: commit any dirty tracked tables ─────────────────────────────────
# bd never creates a Dolt commit for config writes (statutes, memories), so a bare
# push leaves those changes behind and a fresh clone is short however many statutes
# were enacted since the last issue write.
_bp_probe_engine "$DB" \
    || fail_out "cannot determine whether the store has uncommitted changes — no engine could count dolt_status. Refusing to report a push that may leave the statute book behind."

dirty="$(_bp_dirty_count "$DB")" \
    || fail_out "cannot determine whether the store has uncommitted changes (engine: ${_BP_ENGINE:-none})."

if [ "$dirty" -gt 0 ]; then
    if [ "$_BP_ENGINE" = "dolt" ]; then
        commit_out="$(_bp_sql "$DB" \
            "CALL DOLT_COMMIT('-Am', 'beads-push: $stamp');" 2>&1)"; commit_rc=$?
    else
        commit_out="$(bd -C "$DB" dolt commit -m "beads-push: $stamp" 2>&1)"; commit_rc=$?
    fi
    [ "$commit_rc" -eq 0 ] \
        || fail_out "pre-push commit failed: $(tail -2 <<<"$commit_out" | tr '\n' ' ')"

    # A COMMIT THAT CLEARS NOTHING IS A FAILED COMMIT. `bd dolt commit` returned 0 and
    # the words "Nothing to commit." for a working set the other engine could see; taking
    # its exit status alone is what let the statute book sit uncommitted for weeks.
    after="$(_bp_dirty_count "$DB")" \
        || fail_out "committed, but could not re-read the working set to confirm it cleared."
    [ "$after" -eq 0 ] \
        || fail_out "pre-push commit did not clear the working set: $dirty dirty table(s) before, $after after (engine: $_BP_ENGINE)."
    echo "beads-push: $stamp — committed ${dirty} dirty table(s) ($(_bp_statute_count "$DB") statutes)"
fi

# ── PUSH, THEN VERIFY IT BY ITS EFFECT ────────────────────────────────────────
# "Push complete" is a claim about a command, not about the remote. This job exists
# because a configured remote that nothing pushes looks like a backup on inspection;
# a push that reports success without moving the remote is the same lie with an extra
# step. So the remote's head is re-read and compared with the local one, and only that
# comparison is allowed to produce the OK line.
# THE BRANCH IS ASKED FOR, NEVER ASSUMED. A store checked out on anything but main
# would otherwise be verified against a ref that does not move.
branch="$(_bp_sql "$DB" "select active_branch() as b;" 2>/dev/null \
    | tr -d ' |\r' | grep -Ev '^(b|-+|\(.*rows?\)|)$' | head -1)"
[ -n "$branch" ] || branch="main"
out=$(timeout 900 bd -C "$DB" dolt push --remote beads 2>&1)
grep -q 'Push complete' <<<"$out" \
    || fail_out "spira FAILED — $(tail -2 <<<"$out" | tr '\n' ' ')"

# REFRESH THE TRACKING REF FIRST, OR THE COMPARISON BELOW IS MEANINGLESS. remotes/<r>/<b>
# is a local cache; without a fetch it can sit hours behind a remote that did move, and the
# verification would report a false failure.
#
# `dolt --data-dir <dir> fetch <remote>` DOES NOT WORK and does not say so usefully: it
# prints "The current directory is not a valid dolt repository" and still exits 0. The
# database has to be named with --use-db. That silent-success shape is exactly the defect
# this rewrite exists to remove, so the failure text is matched explicitly rather than
# trusting the exit status.
if [ "$_BP_ENGINE" = "dolt" ]; then
    fetch_out="$(dolt --data-dir "$_BP_DATADIR" --use-db "$_BP_DBNAME" \
        fetch beads 2>&1)"; fetch_rc=$?
    case "$fetch_out" in
        *"not a valid dolt repository"*|*"does not exist"*) fetch_rc=1 ;;
    esac
    [ "$fetch_rc" -eq 0 ] \
        || fail_out "pushed, but could not refresh the remote tracking ref to verify it: $(tail -2 <<<"$fetch_out" | tr '\n' ' ')"
fi

local_head="$(_bp_head "$DB" "$branch")"
remote_head="$(_bp_head "$DB" "remotes/beads/$branch")"
[ -n "$local_head" ] && [ -n "$remote_head" ] \
    || fail_out "pushed, but could not read both heads to verify it (local='${local_head:-}' remote='${remote_head:-}'). An unverified push is not a backup."
[ "$local_head" = "$remote_head" ] \
    || fail_out "push reported complete but the remote did not move: local $local_head, remote $remote_head."

echo "beads-push: $stamp — spira OK ($(_bp_statute_count "$DB") statutes, remote at $remote_head)"
exit 0
