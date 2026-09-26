#!/usr/bin/env bash
# full-aeon-fixture.sh — the one full-aeon skeleton (bare origin, clone, chamber, claude
# shim, bd store) that test-aeon-decision-blocked.sh, test-aeon-operator-wait.sh,
# test-requeue.sh, test-aeon-ledger.sh, test-aeon-presession-death.sh,
# test-aeon-yield-headless.sh and test-aeon-exit.sh each built for themselves — 20 suites
# in all did this (docs/test-plan/aeon-execution.md D15). fa_setup builds it once; fa_reset
# resets only the bd store between rows, which is what let those seven pay for a fresh
# `git init --bare` and clone on every one of their combined ~20 aeon runs.
#
# Sourced, never executed, after testlib.sh (for the ok/bad helpers a caller wants) and
# after testdb.sh (fa_setup calls testdb_require/testdb_up/testdb_reset itself, so a
# caller sources testdb.sh first only to pick a mode via SPIRA_TESTDB_MODE). Assumes the
# caller has already set HERE to this directory, the same convention testdb.sh itself uses.
#
#   HERE="$(cd "$(dirname "$0")" && pwd -P)"
#   . "$HERE/testdb.sh"
#   . "$HERE/full-aeon-fixture.sh"
#   fa_setup mysuite
#   trap 'fa_teardown' EXIT INT TERM
#   fa_seed sp-x-1
#   cat > "$FA_BIN/claude" <<'SHIM' ... SHIM
#   fa_run_aeon; is "..." "..." "$(fa_status sp-x-1)"
#
# covers: spira/aeon.sh spira/lib.sh spira/mail.sh
set -uo pipefail

fa_setup() {   # fa_setup <tag> — build the fixture once
    local tag="${1:?fa_setup: tag required}"
    testdb_require "$tag"
    FA_TMP="$(mktemp -d)"
    # A caller that requested server mode and got none (no SPIRA_TESTDB_DATA in this
    # environment) decides for itself whether that is a skip or a failure — return, not exit.
    testdb_up "$tag" || { rm -rf "$FA_TMP"; return 1; }
    export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

    FA_ORIGIN="$FA_TMP/origin.git"; git init -q --bare -b main "$FA_ORIGIN"
    FA_REPO="$FA_TMP/repo"; git clone -q "$FA_ORIGIN" "$FA_REPO" 2>/dev/null
    git -C "$FA_REPO" config user.email t@t; git -C "$FA_REPO" config user.name t
    printf 'seed\n' > "$FA_REPO/f"
    git -C "$FA_REPO" add f; git -C "$FA_REPO" commit -qm seed
    git -C "$FA_REPO" push -q origin main 2>/dev/null

    FA_HOME="$FA_TMP/home"; mkdir -p "$FA_HOME/chamber"
    FA_RUN="$FA_TMP/run"; mkdir -p "$FA_RUN"
    FA_REPO_MAP="$FA_TMP/repo-map"
    export SPIRA_HOME="$FA_HOME" SPIRA_RUN="$FA_RUN" SPIRA_REPO_MAP="$FA_REPO_MAP"
    printf 'fixture | %s | push | origin/main | |\n' "$FA_REPO" > "$SPIRA_REPO_MAP"
    export SPIRA_MAIL="$FA_TMP/mail"
    export SPIRA_MAIL_KINDS="$HERE/mail/kinds"
    cat > "$FA_HOME/chamber/builder.fayth" <<FAYTH
FAYTH_NAME=builder
FAYTH_LABELS="\${SPIRA_SCOPE_LABEL:+\${SPIRA_SCOPE_LABEL},}\${SPIRA_PLAN_LABEL}"
FAYTH_EXCLUDE_LABELS="spira-poison,${SPIRA_ASK_LABEL:-needs-operator}"
FAYTH_MAX_CONCURRENT=1
FAYTH_HEARTBEAT_SECONDS=600
FAYTH
    printf 'work {{BEAD_ID}} in {{REPO}} on {{BRANCH}}\n{{PARK}}\n' > "$FA_HOME/chamber/builder.md"

    FA_BIN="$FA_TMP/bin"; mkdir -p "$FA_BIN"
    export SPIRA_AGENT="$FA_BIN/claude" TMP="$FA_TMP"
    grep -q 'SPIRA_AGENT' "$HERE/aeon.sh" \
        || { printf 'full-aeon-fixture: aeon.sh has no SPIRA_AGENT injection — refusing to run the real model\n' >&2; exit 1; }
}

# fa_teardown — call from the caller's own `trap ... EXIT INT TERM`; not registered here
# so a caller that also needs its own cleanup keeps one trap, not two competing ones.
fa_teardown() { testdb_drop; rm -rf "$FA_TMP"; }

fa_reset() { testdb_reset; }

fa_seed() {   # fa_seed <id> [updated_at]
    local id="$1" updated="${2:-2026-09-04T00:00:00Z}"
    local lbl="${SPIRA_SCOPE_LABEL:+\"${SPIRA_SCOPE_LABEL}\",}\"${SPIRA_PLAN_LABEL:-plan}\",\"repo:fixture\""
    printf '{"id":"%s","title":"t","status":"open","issue_type":"task","labels":[%s],"updated_at":"%s"}\n' \
        "$id" "$lbl" "$updated" | testdb_seed
}

fa_run_aeon() {   # fa_run_aeon [fayth] -> prints aeon.sh's rc; output captured to $FA_TMP/out
    rm -rf "$SPIRA_RUN/worktree"
    "$HERE/aeon.sh" "${1:-builder}" > "$FA_TMP/out" 2>&1
    printf '%s' "$?"
}

fa_out() { cat "$FA_TMP/out" 2>/dev/null; }

fa_field() {   # fa_field <id> <json-field>
    BD_IGNORE_SCHEMA_SKEW=1 bd -C "$SPIRA_DB" show "$1" --json 2>/dev/null \
        | python3 -c '
import sys, json
d = json.load(sys.stdin)
d = d if isinstance(d, list) else [d]
print(d[0].get(sys.argv[1], "") or "")' "$2" 2>/dev/null
}

fa_status() { fa_field "$1" status; }
fa_notes()  { fa_field "$1" notes; }

fa_labels() { bd -C "$SPIRA_DB" label list "$1" 2>/dev/null | tr '\n' ' '; }

fa_ledger_lines() {   # fa_ledger_lines <id> -> every "done builder <id>" line, in order
    grep " done builder $1 " "$SPIRA_RUN/aeon-ledger.log" 2>/dev/null
}

fa_ledger_line() { fa_ledger_lines "$1" | tail -1; }   # the most recent one
