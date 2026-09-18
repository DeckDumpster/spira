#!/usr/bin/env bash
# covers: spira/suites.sh spira/suite-state.sh spira/suite-state
# Flake observer creates a quarantine on a branch, not in the production checkout.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/lib.sh"
. "$HERE/suite-state.sh"

pass() { printf 'ok: %s\n' "$*"; _ok=$((_ok+1)); }
fail() { printf 'FAIL: %s\n' "$*" >&2; _fail=$((_fail+1)); }
_ok=0; _fail=0

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------------------
# POSITIVE CONTROL — prove the test detects a direct write to the checkout.
# A write that bypasses the branch mechanism IS caught by git status --porcelain.
# Without this control, "checkout is clean" could be true because nothing ran.
# ---------------------------------------------------------------------------
_pdir="$TMP/positive-repo"
mkdir -p "$_pdir/spira"
git init -q "$_pdir"
git -C "$_pdir" config user.email "t@t.example"
git -C "$_pdir" config user.name "Test"
: > "$_pdir/spira/suite-state"
git -C "$_pdir" add spira/suite-state
git -C "$_pdir" commit -q --no-gpg-sign -m "init"
suite_state_write "$_pdir/spira/suite-state" "test-fake.sh" quarantined "" "direct"
_d="$(git -C "$_pdir" status --porcelain -- spira/suite-state 2>/dev/null)"
[ -n "$_d" ] && pass "positive control: direct write dirtied the checkout" \
    || fail "positive control: direct write was not detected by git status"

# ---------------------------------------------------------------------------
# FIXTURE — a scratch git repo acts as the harness checkout.
# suites.sh is run from a scratch spira/ directory inside that repo.
# ---------------------------------------------------------------------------
_repo="$TMP/harness"
_sh="$_repo/spira"
_state="$TMP/state"
_run="$TMP/run"
_home="$TMP/home"
mkdir -p "$_sh" "$_state" "$_run" "$_home"

git init -q "$_repo"
git -C "$_repo" config user.email "t@t.example"
git -C "$_repo" config user.name "Test"

for _f in suites.sh lib.sh conf.sh suite-state.sh suite-covers.sh; do
    cp "$HERE/$_f" "$_sh/"
done

# Stub bead.sh: print a valid-looking bead id.
printf '#!/usr/bin/env bash\nprintf "sp-stub99\n"\n' > "$_sh/bead.sh"
chmod +x "$_sh/bead.sh"

# Stub queue.sh: record submit calls, exit 0.
printf '#!/usr/bin/env bash\nprintf "%%s\n" "$*" >> "%s/queue.log"\n' "$TMP" \
    > "$_sh/queue.sh"
chmod +x "$_sh/queue.sh"

# Stub mail.sh: no-op.
printf '#!/usr/bin/env bash\nexit 0\n' > "$_sh/mail.sh"
chmod +x "$_sh/mail.sh"

# Empty gate-suites: all suites are timed (needed by gated_suites()).
: > "$_sh/gate-suites"

# Stub suite: must exist so suite_state_lint and bead.sh reference is valid.
printf '#!/usr/bin/env bash\n: # stub\n' > "$_sh/test-flaky.sh"
chmod +x "$_sh/test-flaky.sh"

# Initial commit with an empty suite-state so the repo has a clean HEAD.
: > "$_sh/suite-state"
git -C "$_repo" add spira/
git -C "$_repo" commit -q --no-gpg-sign -m "init"

# obs — run observe-flake in an explicit minimal environment.
# SPIRA_SUITES_INLINE=1 prevents the batch delegation path; observe-flake never
# runs suites, so the delegation never fires, but declaring it avoids the
# container probe that would reach the host Docker daemon.
obs() {
    env -i \
        PATH="$PATH" \
        HOME="$_home" \
        SPIRA_CONF="$TMP/no.conf" \
        SPIRA_HOME="$_sh" \
        SPIRA_REPO="$_repo" \
        SPIRA_RUN="$_run" \
        SPIRA_SUITES_STATE="$_state" \
        SPIRA_FLAKE_QUARANTINE_AT=2 \
        SPIRA_FLAKE_WINDOW=604800 \
        SPIRA_SUITES_INLINE=1 \
        bash "$_sh/suites.sh" observe-flake test-flaky.sh 2>&1
}

# First observation: below threshold.  Production checkout must stay clean.
obs >/dev/null
_d="$(git -C "$_repo" status --porcelain -- spira/suite-state 2>/dev/null)"
[ -z "$_d" ] && pass "observe(1): production checkout clean below threshold" \
    || fail "observe(1): production checkout dirty after one observation: '$_d'"

# Second observation: crosses threshold.  Quarantine must land on a branch, not in the tree.
obs >/dev/null
_d="$(git -C "$_repo" status --porcelain -- spira/suite-state 2>/dev/null)"
[ -z "$_d" ] && pass "observe(2): production checkout clean after quarantine" \
    || fail "observe(2): production checkout dirty after quarantine: '$_d'"

# A branch carrying the quarantine commit must exist.
_branch="$(git -C "$_repo" branch --list 'spira/suite-state/auto-*' \
    | head -1 | tr -d ' *')"
[ -n "$_branch" ] && pass "observe(2): branch created for quarantine" \
    || fail "observe(2): no branch was created"

if [ -n "$_branch" ]; then
    # The branch must have the quarantine entry in suite-state.
    _tmp="$(mktemp)"
    git -C "$_repo" show "$_branch:spira/suite-state" > "$_tmp" 2>/dev/null
    _st="$(suite_state_of "$_tmp" "test-flaky.sh")"
    rm -f "$_tmp"
    [ "$_st" = "quarantined" ] && pass "observe(2): branch carries quarantine entry" \
        || fail "observe(2): branch suite-state shows '$_st', expected quarantined"

    # queue.sh submit must have been called with the branch name.
    _q="$(cat "$TMP/queue.log" 2>/dev/null || true)"
    case "$_q" in
        *"submit"*"$_branch"*) pass "observe(2): queue submit called with branch" ;;
        *) fail "observe(2): queue submit not called with branch (log: '$_q')" ;;
    esac
fi

printf '\nASSERTIONS %s\n' "$((_ok + _fail))"
[ "$_fail" -eq 0 ] || { printf 'FAILURES: %s\n' "$_fail"; exit 1; }
