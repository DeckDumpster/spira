#!/usr/bin/env bash
# testlib/gate-fixture.sh — the bare-remote-plus-clone that 13 of the 15 gate-verdict
# suites each rebuilt by hand (docs/test-plan/gate-verdict.md section 4, cluster #2).
#
# Sourced, never executed, from a suite that already set HERE to its own directory:
#   . "$HERE/testlib/gate-fixture.sh"
#   gate_fixture_init "$TMP"          # populates REPO REMOTE RUN SH MAP
#   gate_fixture_branch spira/sp-t1   # one commit off origin/main
#   gate_fixture_run "$BR" repo       # runs the copied gate.sh in a fixed, minimal env
#
# THE ENV IS ALWAYS SPIRA_CONF=<nonexistent>. Some suites this replaces set SPIRA_HOME
# instead and left SPIRA_CONF unset, so conf.sh's default search order could still find
# an ambient spira.conf and the suite's verdict would depend on the box it ran on
# (law-run-in-explicit-minimal-environment). Pointing SPIRA_CONF at a path that cannot
# exist is the one setting that rules that out regardless of what else is exported.
gate_fixture_init() {
    local tmp="$1"
    REPO="$tmp/repo"; REMOTE="$tmp/remote.git"; RUN="$tmp/run"; SH="$tmp/spira"
    MAP="$tmp/repo-map"; HOMEDIR="$tmp/home"; SPIRA_CONF_NONE="$tmp/nonexistent.conf"
    SPIRA_DB_NONE="$tmp/nonexistent-db"; GATELOG="$tmp/gate.log"; VDIR="$tmp/verdicts"
    mkdir -p "$RUN/worktree" "$HOMEDIR" "$SH"
    cp "$HERE/gate.sh" "$HERE/gate-lib.sh" "$HERE/lib.sh" "$HERE/conf.sh" "$HERE/exclude.sh" \
       "$HERE/skew.sh" "$HERE/yield.sh" "$HERE/suite-covers.sh" "$HERE/gate-sweep.sh" "$SH/"
    export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
    git init -q --bare -b main "$REMOTE"
    git init -q -b main "$REPO"
    printf 'base\n' > "$REPO/marker"
    git -C "$REPO" add -A; git -C "$REPO" commit -q -m base
    git -C "$REPO" remote add origin "$REMOTE"
    git -C "$REPO" push -q origin main; git -C "$REPO" fetch -q origin
    printf 'repo | %s | push | origin/main |  | true\n' "$REPO" > "$MAP"
}

# gate_fixture_branch <branch> [file] [content] — one commit off origin/main, in REPO.
gate_fixture_branch() {
    local br="$1" file="${2:-f1.txt}" content="${3:-work}" w
    w="$(mktemp -d)"
    git -C "$REPO" worktree add -q -b "$br" "$w" origin/main
    printf '%s\n' "$content" > "$w/$file"
    git -C "$w" add -A; git -C "$w" commit -q -m "feat: $br"
    git -C "$REPO" worktree remove --force "$w"
}

# gate_fixture_run <branch> <repo-name> [VAR=VAL ...] — the copied gate.sh, in the fixed env.
gate_fixture_run() {
    local br="$1" repo="$2"; shift 2
    env -i HOME="$HOMEDIR" PATH="/usr/bin:/bin" \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        SPIRA_CONF="$SPIRA_CONF_NONE" SPIRA_REPO="$REPO" SPIRA_RUN="$RUN" \
        SPIRA_DB="$SPIRA_DB_NONE" SPIRA_REPO_MAP="$MAP" \
        SPIRA_GATE_LOG="$GATELOG" SPIRA_VERDICTS="$VDIR" \
        SPIRA_VERDICT_TTL=0 \
        "$@" bash "$SH/gate.sh" "$br" "$repo" 2>&1
}
