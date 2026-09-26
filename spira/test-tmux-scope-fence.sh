#!/usr/bin/env bash
#
# test-tmux-scope-fence.sh — positive control for the tmux-socket scope fence.
#
#   ./test-tmux-scope-fence.sh
#
# THE DEFECT THIS GUARDS AGAINST. A suite that calls a tmux session command, or runs
# concierge.sh start/wake/here/stop, without scoping it to a socket of its own reaches
# whatever tmux server the caller's environment already points at — the operator's own,
# on the host, twice (sp-pfca0). tmux-scope-fence.sh refuses any such suite.
#
# THE POSITIVE CONTROL IS THE FIRST ASSERTION. A fence that reports a clean tree is
# indistinguishable from one whose matcher never fires. An offender is planted in a
# scratch repository, the fence is required to name it (SEEN RED), the plant is
# withdrawn, and only then is the fence's silence evidence of a clean tree
# (law-absence-needs-a-positive-control).
#
# host-reason: tests tmux-scope-fence.sh against scratch git repositories only
#
# covers: spira/tmux-scope-fence.sh spira/gate-spira.sh spira/gate-fences.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-tmux-scope-fence.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

fence_at() {
    local root="$1"
    env -i PATH="$PATH" HOME="$TMP" TERM=dumb bash "$root/spira/tmux-scope-fence.sh" 2>&1
}

# ---------------------------------------------------------------------------------------
# EMPTY TREE — must exit 3 (law-absence-needs-a-positive-control).
# ---------------------------------------------------------------------------------------
ROOT="$TMP/root"; mkdir -p "$ROOT/spira"
cp "$HERE/tmux-scope-fence.sh" "$ROOT/spira/tmux-scope-fence.sh"
git init -q -b main "$ROOT"
git -C "$ROOT" config user.email t@t; git -C "$ROOT" config user.name t
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m init

out="$(fence_at "$ROOT")"; rc=$?
is   "no test-*.sh files yet exits 3" "3" "$rc"
want "and says why"                   "refusing to report clean" "$out"

# Seed one clean suite so subsequent checks have a non-empty index.
printf '#!/usr/bin/env bash\necho ok\n' > "$ROOT/spira/test-clean.sh"
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "seed clean suite"

out="$(fence_at "$ROOT")"; rc=$?
is   "clean tree exits 0" "0" "$rc"
want "and says so"         "no unscoped tmux" "$out"

# ---------------------------------------------------------------------------------------
# SEEN RED: a bare tmux kill-server with no -L and no file-wide TMUX_TMPDIR.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-broken-tmux.sh" << 'BROKEN'
#!/usr/bin/env bash
tmux kill-server
BROKEN
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant bare kill-server"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: bare kill-server refused" "1" "$rc"
want "names the offending file"           "test-broken-tmux.sh" "$out"
want "names the offending line"           "tmux kill-server" "$out"

git -C "$ROOT" rm -qf spira/test-broken-tmux.sh
git -C "$ROOT" commit -q -m "withdraw bare kill-server"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean after withdrawal" "0" "$rc"

# ---------------------------------------------------------------------------------------
# POSITIVE CONTROL WITHIN THE POSITIVE CONTROL: -L scoping clears the same line.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-scoped-tmux.sh" << 'SCOPED'
#!/usr/bin/env bash
SOCK="my-scoped-socket-$$"
tmux -L "$SOCK" kill-server 2>/dev/null || true
SCOPED
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant -L scoped kill-server"

out="$(fence_at "$ROOT")"; rc=$?
is   "a -L scoped kill-server is not flagged" "0" "$rc"

git -C "$ROOT" rm -qf spira/test-scoped-tmux.sh
git -C "$ROOT" commit -q -m "withdraw scoped suite"

# ---------------------------------------------------------------------------------------
# TMUX_TMPDIR ANYWHERE IN THE FILE CLEARS EVERY BARE CALL IN IT — the shipped pattern
# (test-cockpit-layout.sh: one export, many bare calls afterward).
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-tmpdir-scoped.sh" << 'TDIR'
#!/usr/bin/env bash
export TMUX_TMPDIR="$(mktemp -d)"
tmux start-server
tmux new-session -d -s cockpit
tmux kill-server 2>/dev/null || true
TDIR
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant TMUX_TMPDIR-scoped suite"

out="$(fence_at "$ROOT")"; rc=$?
is   "file-wide TMUX_TMPDIR clears bare calls in the same file" "0" "$rc"

git -C "$ROOT" rm -qf spira/test-tmpdir-scoped.sh
git -C "$ROOT" commit -q -m "withdraw TMUX_TMPDIR-scoped suite"

# ---------------------------------------------------------------------------------------
# SEEN RED: concierge.sh start/wake/here/stop with no CONCIERGE_SOCKET or
# CONCIERGE_SESSION anywhere in the file — this is the class that killed the operator's
# real concierge session, since concierge.sh's own socket defaults to "concierge".
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-broken-concierge.sh" << 'BROKEN'
#!/usr/bin/env bash
HARNESS="$(cd "$(dirname "$0")/.." && pwd)"
bash "$HARNESS/concierge.sh" start
BROKEN
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant unscoped concierge.sh start"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN RED: unscoped concierge.sh start refused" "1" "$rc"
want "names the offending file"                      "test-broken-concierge.sh" "$out"
want "names concierge.sh"                             "concierge.sh" "$out"

git -C "$ROOT" rm -qf spira/test-broken-concierge.sh
git -C "$ROOT" commit -q -m "withdraw unscoped concierge.sh start"

out="$(fence_at "$ROOT")"; rc=$?
is   "SEEN GREEN: clean after withdrawing the concierge offender" "0" "$rc"

# ---------------------------------------------------------------------------------------
# POSITIVE CONTROL: CONCIERGE_SOCKET anywhere in the file clears the invocation.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-scoped-concierge.sh" << 'SCOPED'
#!/usr/bin/env bash
HARNESS="$(cd "$(dirname "$0")/.." && pwd)"
CONCIERGE_SOCKET="test-only-$$" CONCIERGE_SESSION="test-only-$$" \
    bash "$HARNESS/concierge.sh" start
SCOPED
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant scoped concierge.sh start"

out="$(fence_at "$ROOT")"; rc=$?
is   "a CONCIERGE_SOCKET-scoped start is not flagged" "0" "$rc"

git -C "$ROOT" rm -qf spira/test-scoped-concierge.sh
git -C "$ROOT" commit -q -m "withdraw scoped concierge suite"

# ---------------------------------------------------------------------------------------
# COMMENT LINES ARE NOT FLAGGED. Documentation of the defect must not trigger the fence.
# ---------------------------------------------------------------------------------------
cat > "$ROOT/spira/test-comment-only.sh" << 'SAFE'
#!/usr/bin/env bash
# do not call: tmux kill-server
# and never: bash "$HARNESS/concierge.sh" start
printf 'ok\n'
SAFE
git -C "$ROOT" add .; git -C "$ROOT" commit -q -m "plant comment-only reference"

out="$(fence_at "$ROOT")"; rc=$?
is   "comment lines are not flagged" "0" "$rc"

git -C "$ROOT" rm -qf spira/test-comment-only.sh
git -C "$ROOT" commit -q -m "remove comment-only suite"

# ---------------------------------------------------------------------------------------
# GATE INTEGRATION: the fence is wired into gate-spira.sh via gate_fence_list, and every
# entry that list names is a real file — the same check test-gate-fences.sh runs, repeated
# here so this row does not depend on that one to catch a dropped wiring.
# ---------------------------------------------------------------------------------------
if grep -q "tmux-scope-fence" "$HERE/gate-spira.sh" 2>/dev/null; then
    ok "gate-spira.sh references tmux-scope-fence.sh"
else
    bad "gate-spira.sh references tmux-scope-fence.sh" "not found in gate-spira.sh"
fi
if grep -q "spira/tmux-scope-fence.sh" "$HERE/gate-fences.sh" 2>/dev/null; then
    ok "gate-fences.sh lists tmux-scope-fence.sh"
else
    bad "gate-fences.sh lists tmux-scope-fence.sh" "not found in gate_fence_list"
fi

# ---------------------------------------------------------------------------------------
# THE SHIPPED TREE ITSELF IS CLEAN — the fence runs against this repository's real
# spira/test-*.sh corpus and must find nothing, proving the matcher is not merely
# well-behaved on fixtures but agrees with the tree it actually gates.
# ---------------------------------------------------------------------------------------
shipped_out="$(bash "$HERE/tmux-scope-fence.sh" 2>&1)"; shipped_rc=$?
is "the shipped suite corpus is clean" "0" "$shipped_rc"
want "and says so" "no unscoped tmux" "$shipped_out"

tl_summary
