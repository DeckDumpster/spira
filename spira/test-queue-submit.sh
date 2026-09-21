#!/usr/bin/env bash
# test-queue-submit.sh — queue.sh submit refuses branches the batcher cannot adopt.
#
# In queue mode, batch.sh builds only from refs/heads/spira/<id>. A branch not
# in that form must be refused before any state is written. The queue-dir write's
# exit status must also be checked so a write failure does not silently produce
# a "certified" result.
#
# covers: spira/queue.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()     { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()    { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "$2"; }
want()   { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }
nowant() { [[ "$3" != *"$2"* ]] && ok "$1" || bad "$1" "did not want [$2] in [$3]"; }

echo "test-queue-submit.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

GATE_LOG="$TMP/gate-calls.log"

cp -r "$HERE" "$TMP/spira"
cat > "$TMP/spira/gate.sh" <<FAKE
#!/usr/bin/env bash
printf 'gate-called branch=%s\n' "\${1:-}" >> "$GATE_LOG"
exit 0
FAKE
chmod +x "$TMP/spira/gate.sh"

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m "init"

RMAP="$TMP/repo-map"
printf 'fixq | %s | queue | main | | |\n' "$REPO" > "$RMAP"

run() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME_REPO=fixq \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_QUEUE_DIR="$TMP/run/queue" \
        SPIRA_REPO_MAP="$RMAP" \
        bash "$TMP/spira/queue.sh" "$@" 2>&1
}

git -C "$REPO" branch "spira/sp-abc01" main
git -C "$REPO" branch "spira-suite-state/test-foo-20260101000000" main

echo
echo "positive control — gate is reachable for a valid spira/<id> branch:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
: > "$GATE_LOG"
out="$(run submit spira/sp-abc01)"; rc=$?
want "gate stub was called for valid branch" "gate-called" "$(cat "$GATE_LOG")"
want "exit 0 for valid branch" "certified" "$out"
rm -rf "$TMP/run"

echo
echo "spira-suite-state/... transition branch is accepted (not refused like non-spira/):"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
: > "$GATE_LOG"
out="$(run submit spira-suite-state/test-foo-20260101000000)"; rc=$?
want "gate stub was called for suite-state branch" "gate-called" "$(cat "$GATE_LOG")"
want "suite-state branch certified" "certified" "$out"
[ "$rc" -eq 0 ] && ok "exit 0 for suite-state branch" || bad "exit 0 for suite-state branch" "got rc=$rc"
rm -rf "$TMP/run"

echo
echo "submit refuses a non-spira/ branch in queue mode:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
git -C "$REPO" branch "concierge/sp-swux6" main
out="$(run submit "concierge/sp-swux6")"; rc=$?
[ "$rc" -ne 0 ] && ok "exits non-zero" || bad "exits non-zero" "got rc=$rc"
want   "names required form" "spira/" "$out"
nowant "never says certified" "certified"   "$out"
[ ! -e "$TMP/run/landstate/concierge" ] \
    && ok "no landstate subdir created" \
    || bad "no landstate subdir created" "directory exists"
[ ! -e "$TMP/run/queue/concierge" ] \
    && ok "no queue subdir created" \
    || bad "no queue subdir created" "directory exists"
rm -rf "$TMP/run"

echo
echo "submit allows a spira-suite-state/* transition branch:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
git -C "$REPO" branch "spira-suite-state/test-q.sh" main
: > "$GATE_LOG"
out="$(run submit spira-suite-state/test-q.sh)"; rc=$?
want "exit 0 for transition branch" "certified" "$out"
want "gate was called for transition branch" "gate-called" "$(cat "$GATE_LOG")"
rm -rf "$TMP/run"

echo
echo "submit uses the given repo name, not the home repo, for mode and gate:"
REPO2="$TMP/repo2"
git init -q -b main "$REPO2"
git -C "$REPO2" commit -q --allow-empty -m "init"
git -C "$REPO2" branch "spira/sp-def02" main

GATE_LOG2="$TMP/gate-calls2.log"
cp -r "$HERE" "$TMP/spira2"
cat > "$TMP/spira2/gate.sh" <<FAKE2
#!/usr/bin/env bash
printf 'gate-called branch=%s repo=%s\n' "\${1:-}" "\${2:-}" >> "$GATE_LOG2"
exit 0
FAKE2
chmod +x "$TMP/spira2/gate.sh"

RMAP2="$TMP/repo-map2"
printf 'holdhome | %s | hold | main | | |\n' "$REPO" > "$RMAP2"
printf 'queuerepo | %s | queue | main | | |\n' "$REPO2" >> "$RMAP2"

mkdir -p "$TMP/run2/queue" "$TMP/run2/landstate"
: > "$GATE_LOG2"
run2() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME_REPO=holdhome \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run2" \
        SPIRA_QUEUE_DIR="$TMP/run2/queue" \
        SPIRA_REPO_MAP="$RMAP2" \
        bash "$TMP/spira2/queue.sh" "$@" 2>&1
}

out="$(run2 submit spira/sp-def02 queuerepo)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 when repo arg names a queue-mode repo" \
    || bad "exit 0 when repo arg names a queue-mode repo" "got rc=$rc out=$out"
want "gate called with queue repo name" "repo=queuerepo" "$(cat "$GATE_LOG2")"
want "certified via named repo" "certified" "$out"
rm -rf "$TMP/run2"

echo
echo "submit uses home repo when no second arg — gate sees home repo name:"
mkdir -p "$TMP/run3/queue" "$TMP/run3/landstate"
: > "$GATE_LOG2"
git -C "$REPO" branch "spira/sp-ghi03" main 2>/dev/null || true

RMAP3="$TMP/repo-map3"
printf 'fixq | %s | queue | main | | |\n' "$REPO" > "$RMAP3"

run3() {
    env -i PATH="/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP" \
        SPIRA_CONF=/nonexistent \
        SPIRA_HOME_REPO=fixq \
        SPIRA_REPO="$REPO" \
        SPIRA_RUN="$TMP/run3" \
        SPIRA_QUEUE_DIR="$TMP/run3/queue" \
        SPIRA_REPO_MAP="$RMAP3" \
        bash "$TMP/spira2/queue.sh" "$@" 2>&1
}
out="$(run3 submit spira/sp-ghi03)"; rc=$?
[ "$rc" -eq 0 ] && ok "exit 0 with no repo arg" || bad "exit 0 with no repo arg" "got rc=$rc out=$out"
want "gate sees home repo name when no arg" "repo=fixq" "$(cat "$GATE_LOG2")"
rm -rf "$TMP/run3"

echo
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
