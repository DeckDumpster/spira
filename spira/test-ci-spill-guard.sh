#!/usr/bin/env bash
# test-ci-spill-guard.sh — a provision step that is not spill: ec2-only is found by file and line,
# by the gate fence and by the watcher over a repo's landing ref, once per violation.
#
# tier: T1
# covers: spira/ci-spill-guard.sh spira/watchers gate.steps .github/workflows/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-ci-spill-guard.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
G="$HERE/ci-spill-guard.sh"

wf() {   # wf <tree> <name> <spill-line-or-empty>
    mkdir -p "$1/.github/workflows"
    {
        printf 'name: x\njobs:\n  provision:\n    steps:\n      - uses: acme/ci/provision@v2\n        with:\n          runner: a\n'
        [ -n "$3" ] && printf '          %s\n' "$3"
        printf '          spill-after-seconds: 600\n      - run: true\n'
    } > "$1/.github/workflows/$2"
}

wf "$TMP/bad" ci.yml "spill: ec2"
out="$(bash "$G" --gate "$TMP/bad" 2>&1)"; rc=$?
is "spill: ec2 fails the fence" 1 "$rc"
want "the report names file and line" ".github/workflows/ci.yml:8: provision spill: ec2" "$out"
want "the report names the override" "override: spill: ec2-only" "$out"

wf "$TMP/none" ci.yml ""
out="$(bash "$G" scan "$TMP/none" 2>&1)"; rc=$?
is "an absent spill: fails" 1 "$rc"
want "an absent spill: is named" "no spill: input" "$out"

wf "$TMP/exp" ci.yml "spill: \${{ inputs.x && 'ec2-only' || 'ec2' }}"
bash "$G" scan "$TMP/exp" >/dev/null 2>&1; is "an expression is not ec2-only" 1 "$?"

wf "$TMP/good" ci.yml "spill: ec2-only  # always"
out="$(bash "$G" --gate "$TMP/good" 2>&1)"; rc=$?
is "ec2-only passes" 0 "$rc"
want "the fence says what it checked" "fence: ci-spill-guard checked 1 provision steps" "$out"

mkdir -p "$TMP/empty"
bash "$G" --gate "$TMP/empty" >/dev/null 2>&1; is "a tree with nothing to check is refused" 1 "$?"

mkdir -p "$TMP/two/.github/workflows"
cp "$TMP/good/.github/workflows/ci.yml" "$TMP/two/.github/workflows/a.yml"
cat >> "$TMP/two/.github/workflows/a.yml" <<'Y'
  second:
    steps:
      - uses: acme/ci/provision@v2
        with:
          spill: ec2
Y
out="$(bash "$G" scan "$TMP/two" 2>&1)"
want "a second provision step in a file is judged on its own" "a.yml:" "$out"
nowant "the compliant first step is not reported" "a.yml:8" "$out"

echo "watcher"
REPO="$TMP/repo"; git init -q "$REPO"
wf "$REPO" ci.yml "spill: ec2"
git -C "$REPO" add -A; git -C "$REPO" -c user.name=t -c user.email=t@t commit -qm init
printf 'fix | %s | queue.local | main | | true\n' "$REPO" > "$TMP/map"
INBOX="$TMP/inbox.log"; mkdir -p "$TMP/run"
tl_config SPIRA_REPO_MAP="$TMP/map" SPIRA_RUN="$TMP/run" SPIRA_CONCIERGE_INBOX="$INBOX"
w() { env -i PATH="$PATH" HOME="$TMP" SPIRA_TOML="$SPIRA_TOML" SPIRA_REPO_MAP="$TMP/map" bash "$G" "$@" 2>&1; }
want "the watcher reports the violation" "ci.yml:8" "$(w pass)"
want "the inbox gets it" "CI SPILL" "$(cat "$INBOX" 2>/dev/null)"
is "the same violation is said once" "" "$(w pass)"
wf "$REPO" ci.yml "spill: ec2-only"
git -C "$REPO" add -A; git -C "$REPO" -c user.name=t -c user.email=t@t commit -qm fix
is "a repaired repo is quiet" "" "$(w pass)"

tl_summary
