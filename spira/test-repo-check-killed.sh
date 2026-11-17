#!/usr/bin/env bash
# test-repo-check-killed.sh — a repo-check run SIGKILLed mid-run leaves its worktree under
# $SPIRA_RUN/worktree, and the next cut reaps it; nothing is ever cut in the system temp dir.
#
# tier: T1
# covers: spira-config/src/repo_admin.rs repo-contract/src/* spira/test-repo-check-killed.sh
# timeout: 300
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-repo-check-killed.sh"

RC="$(command -v repo-contract)" || bail "repo-contract not on PATH"
SC="$(command -v spira-config)" || bail "spira-config not on PATH"

REPO="$TMPDIR/fixture"; RUN="$TMPDIR/run"; SCRATCH="$TMPDIR/scratch"
mkdir -p "$REPO/.spira" "$RUN" "$SCRATCH"
cat > "$REPO/.spira/checks.toml" <<'TOML'
[[check]]
name = "sleeps"
run = "sleep 600"
budget_ms = 900000
TOML
git -C "$REPO" init -q
git -C "$REPO" add -A
git -C "$REPO" -c user.email=t@t -c user.name=t commit -qm init
git -C "$REPO" branch -f local/main HEAD

tl_repo_map "fixture | $REPO | local | local/main | |"
CONF="$(tl_layer SPIRA_RUN="$RUN")"
run() { env TMPDIR="$SCRATCH" SPIRA_RELEASE="$SPIRA_RELEASE" SPIRA_TOML="$CONF" PATH="$PATH" HOME="$HOME" "$@"; }

run "$RC" check fixture >/dev/null 2>&1 &
pid=$!
tree=""
for _ in $(seq 1 600); do
    tree="$(ls -d "$RUN"/worktree/repo-check-*-* 2>/dev/null | head -1)"
    [ -n "$tree" ] && break
    sleep 0.1 # waits on the child's own marker directory appearing
done
[ -n "$tree" ] || bad "the run cut a repo-check tree under the run dir" "none under $RUN/worktree"
killed="$(basename "${tree:-repo-check-0-0}" | cut -d- -f3)"
pkill -KILL -P "$pid" 2>/dev/null
kill -KILL "$pid" 2>/dev/null
wait "$pid" 2>/dev/null
[ -d "$tree" ] && ok "the killed run left its tree behind" || bad "the killed run left its tree behind" "$tree"

out="$(run "$SC" repo check fixture 2>&1)"
want "repo check still reports the worktree piece" "cut from local/main" "$out"
want "the reap is announced" "repo-check: reaped" "$out"
if [ -d "$tree" ]; then bad "the killed run's tree is reaped by the next cut" "$tree"; else ok "the killed run's tree is reaped by the next cut"; fi
left="$(ls "$RUN"/worktree 2>/dev/null | grep "^repo-check-$killed-" || true)"
is "no repo-check-<killed pid> remains" "" "$left"
tmpleft="$(ls "$SCRATCH" /tmp 2>/dev/null | grep '^spira-repo-check-' || true)"
is "nothing named spira-repo-check-* is cut in the temp dir" "" "$tmpleft"

tl_summary
