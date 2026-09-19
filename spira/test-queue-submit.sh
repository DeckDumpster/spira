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
git -C "$REPO" branch "spira/suite-state/test-foo-20260101000000" main

echo
echo "positive control — gate is reachable for a valid spira/<id> branch:"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
: > "$GATE_LOG"
out="$(run submit spira/sp-abc01)"; rc=$?
want "gate stub was called for valid branch" "gate-called" "$(cat "$GATE_LOG")"
want "exit 0 for valid branch" "certified" "$out"
rm -rf "$TMP/run"

echo
echo "spira/suite-state/... transition branch is accepted (not refused like non-spira/):"
mkdir -p "$TMP/run/queue" "$TMP/run/landstate"
: > "$GATE_LOG"
out="$(run submit spira/suite-state/test-foo-20260101000000)"; rc=$?
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
printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
