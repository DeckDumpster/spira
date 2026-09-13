#!/usr/bin/env bash
#
# test-promote.sh — promote.sh names its model and behaves accordingly.
#
# WHAT THIS SUITE IS FOR
# ----------------------
# promote.sh supports two installation layouts: split-checkout (SPIRA_PROD outside
# SPIRA_REPO) and single-checkout (SPIRA_PROD inside SPIRA_REPO). The latter is the
# case where the dev/prod separation does not exist, and promote.sh must not silently
# do nothing — it exits non-zero and names the right tool (skew.sh refresh).
#
# THE INVARIANT UNDER OPTION B (single-checkout model):
# promote.sh --dry-run exits non-zero when SPIRA_PROD is inside SPIRA_REPO, and the
# error output names the condition. "Exits non-zero doing nothing" is the bug this
# closes; the fix is an explicit, named exit.
#
# THE POSITIVE CONTROL IS FIRST. Before asserting that the fixed promote.sh names the
# condition correctly, prove that a version WITHOUT the fix would fail the invariant —
# i.e., that the invariant is discriminating and not trivially satisfied.
#
# Also tested here: the live-aeon guard (sp-sb5ss). promote.sh refuses to reset the
# production checkout while spira-aeon-* units for the current instance are active.
#
# covers: spira/promote.sh spira/lib.sh spira/conf.sh
# scar: promote.sh silently exited 0 in single-checkout mode (SPIRA_PROD inside SPIRA_REPO) instead of naming the condition and exiting non-zero.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
pass=0; fail=0
ok()   { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()  { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
want() { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-promote.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
# Git identity for fixture commits — required in the container (no ~/.gitconfig).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

# A minimal git repo doubles as both SPIRA_REPO and a fake SPIRA_PROD parent.
REPO="$TMP/repo"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" commit --allow-empty -m "init" -q

# A real conf.sh and lib.sh are needed so promote.sh can source lib.sh -> conf.sh.
# We link the real ones so the detection logic they contain stays live.
mkdir -p "$REPO/spira"
ln -s "$HERE/lib.sh"  "$REPO/spira/lib.sh"
ln -s "$HERE/conf.sh" "$REPO/spira/conf.sh"
printf '# empty\n' > "$REPO/spira/repo-map.example"
printf '# empty\n' > "$REPO/spira/watchers"

# ==========================================================================
echo
echo "positive control — old promote.sh (pre-fix) silently exits 0 in single-checkout mode:"
# ==========================================================================
# A promote.sh that has no single-checkout detection would fast-forward PROD_REPO ==
# dirname(SPIRA_PROD). When SPIRA_PROD is inside SPIRA_REPO, PROD_REPO is also inside
# SPIRA_REPO, and the script would attempt git operations against itself. In practice the
# pre-fix code: (a) resolves the ref, (b) checks if PROD_REPO/.git exists, (c) clones if
# not. Since PROD_REPO (dirname of SPIRA_PROD = SPIRA_REPO) has a .git, it goes into the
# fast-forward path and exits 0 after printing "nothing to do" or advancing the HEAD —
# treating the repo AS the prod checkout. This is the silent incorrect behaviour the fix
# closes.
#
# We recreate the pre-fix behaviour by building a stripped promote.sh that omits the
# single-checkout block. The invariant check then asserts that this OLD version exits 0
# — proving the invariant would have failed before the fix.
OLD_PROMOTE="$TMP/old-promote.sh"
# Strip the single-checkout detection block from the real promote.sh. Everything between
# "_promote_prod=" and "unset _promote_prod" is the detection block.
sed '/^_promote_prod=/,/^unset _promote_prod/d' "$HERE/promote.sh" > "$OLD_PROMOTE"
chmod +x "$OLD_PROMOTE"

# Single-checkout configuration: SPIRA_PROD is inside SPIRA_REPO.
PROD_INSIDE="$REPO/spira"   # <-- inside the repo

old_rc=0
env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$REPO/spira" \
    SPIRA_REPO="$REPO" \
    SPIRA_PROD="$PROD_INSIDE" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$REPO/spira/watchers" \
    bash "$OLD_PROMOTE" --dry-run HEAD 2>/dev/null
old_rc=$?
# The pre-fix code exits 0 (nothing to do, already at HEAD). That is the WRONG behaviour.
if [ "$old_rc" -eq 0 ]; then
    ok "positive control: pre-fix promote.sh exits 0 in single-checkout mode (this is the bug)"
else
    bad "positive control: pre-fix promote.sh exits 0 in single-checkout mode" \
        "expected rc=0, got rc=$old_rc"
fi

# ==========================================================================
echo
echo "invariant — fixed promote.sh exits non-zero in single-checkout mode:"
# ==========================================================================
err_out=""
rc=0
err_out="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$REPO/spira" \
    SPIRA_REPO="$REPO" \
    SPIRA_PROD="$PROD_INSIDE" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$REPO/spira/watchers" \
    bash "$HERE/promote.sh" --dry-run HEAD 2>&1)" || rc=$?

if [ "$rc" -ne 0 ]; then
    ok "fixed promote.sh exits non-zero in single-checkout mode (rc=$rc)"
else
    bad "fixed promote.sh exits non-zero in single-checkout mode" "exited 0 — silently did nothing"
fi

want "error output names single-checkout mode" "single-checkout" "$err_out"
want "error output names skew.sh refresh"      "skew.sh refresh" "$err_out"

# ==========================================================================
echo
echo "split-checkout mode — promote.sh does NOT exit early when SPIRA_PROD is outside SPIRA_REPO:"
# ==========================================================================
# When SPIRA_PROD is outside SPIRA_REPO, the single-checkout gate must not fire.
# Verify that by confirming the error message for single-checkout is absent in that case.
# (The script will fail for other reasons — no real prod clone — but not for single-checkout.)
PROD_OUTSIDE="$TMP/prod/spira"   # outside the repo
mkdir -p "$(dirname "$PROD_OUTSIDE")"

err_out2=""
env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$REPO/spira" \
    SPIRA_REPO="$REPO" \
    SPIRA_PROD="$PROD_OUTSIDE" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$REPO/spira/watchers" \
    bash "$HERE/promote.sh" --dry-run HEAD 2>&1 | head -5 > /dev/null
err_out2="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$REPO/spira" \
    SPIRA_REPO="$REPO" \
    SPIRA_PROD="$PROD_OUTSIDE" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$REPO/spira/watchers" \
    bash "$HERE/promote.sh" --dry-run HEAD 2>&1 || true)"

if [[ "$err_out2" != *"single-checkout"* ]]; then
    ok "single-checkout gate does not fire when SPIRA_PROD is outside SPIRA_REPO"
else
    bad "single-checkout gate does not fire when SPIRA_PROD is outside SPIRA_REPO" \
        "found 'single-checkout' in output: $err_out2"
fi

# ==========================================================================
echo
echo "symlink case — detection works when SPIRA_PROD is a symlink into SPIRA_REPO:"
# ==========================================================================
LINK_PROD="$TMP/link-prod"
ln -s "$REPO/spira" "$LINK_PROD"   # symlink resolves to inside REPO

sym_err=""
sym_rc=0
sym_err="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$REPO/spira" \
    SPIRA_REPO="$REPO" \
    SPIRA_PROD="$LINK_PROD" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$REPO/spira/watchers" \
    bash "$HERE/promote.sh" --dry-run HEAD 2>&1)" || sym_rc=$?

if [ "$sym_rc" -ne 0 ]; then
    ok "single-checkout detection works through a symlink (rc=$sym_rc)"
else
    bad "single-checkout detection works through a symlink" "exited 0 — symlink defeated the check"
fi
want "symlink error names single-checkout" "single-checkout" "$sym_err"

# ==========================================================================
echo
echo "fetch-before-resolve — local branch stale, origin/main is ahead, prod needs advancing:"
# ==========================================================================
# WHAT THIS TESTS. The timer runs 'promote.sh origin/main' from the dev checkout. The dev
# checkout's local 'main' is typically stale (the landing pass pushes to origin without
# fast-forwarding the local branch). If promote.sh does not fetch before resolving the ref,
# 'origin/main' resolves to the stale local ref, which may be behind the production checkout
# — causing every timer tick to report "not a fast-forward" and stall.
#
# Set up: a dev repo whose local 'main' is at A; a bare 'origin' at B (B descends from A);
# a prod checkout at A. Verify that 'promote.sh --dry-run origin/main' exits 0 even though
# local 'main' == prod HEAD, by proving it fetched and resolved origin/main to B.
FF_DEV="$TMP/ff-dev"
FF_ORIGIN="$TMP/ff-origin.git"
FF_PROD="$TMP/ff-prod"
mkdir -p "$FF_DEV/spira"

# Bootstrap: dev repo with an initial commit A on main.
git -C "$TMP" init -q ff-dev 2>/dev/null
git -C "$FF_DEV" commit --allow-empty -m "commit-A" -q
COMMIT_A="$(git -C "$FF_DEV" rev-parse HEAD)"

# Bare origin cloned from dev at commit A.
git clone --bare -q "$FF_DEV" "$FF_ORIGIN"
git -C "$FF_DEV" remote add origin "$FF_ORIGIN"

# Advance origin to commit B (one commit ahead of A).
git -C "$FF_DEV" checkout -q -b tmp-branch
git -C "$FF_DEV" commit --allow-empty -m "commit-B" -q
git -C "$FF_DEV" push origin tmp-branch:main -q 2>/dev/null
git -C "$FF_DEV" checkout -q main
git -C "$FF_DEV" branch -D tmp-branch -q 2>/dev/null || true
COMMIT_B="$(git -C "$FF_ORIGIN" rev-parse refs/heads/main)"

# Prod checkout pinned at A (local main is stale; origin/main is at B).
git clone -q "$FF_ORIGIN" "$FF_PROD" 2>/dev/null
git -C "$FF_PROD" checkout --detach -q "$COMMIT_A"

# Symlink lib.sh and conf.sh so promote.sh can source them from the dev fixture.
ln -sf "$HERE/lib.sh"  "$FF_DEV/spira/lib.sh"
ln -sf "$HERE/conf.sh" "$FF_DEV/spira/conf.sh"
printf '# empty\n' > "$FF_DEV/spira/repo-map.example"
printf '# empty\n' > "$FF_DEV/spira/watchers"

ff_err="" ff_rc=0
ff_err="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$FF_DEV/spira" \
    SPIRA_REPO="$FF_DEV" \
    SPIRA_PROD="$FF_PROD/spira" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$FF_DEV/spira/watchers" \
    SPIRA_SYSTEMCTL=/bin/true \
    bash "$HERE/promote.sh" --dry-run origin/main 2>&1)" || ff_rc=$?

if [ "$ff_rc" -eq 0 ]; then
    ok "fetch-before-resolve: promote.sh --dry-run origin/main exits 0 when local main is stale"
else
    bad "fetch-before-resolve: promote.sh --dry-run origin/main exits 0 when local main is stale" \
        "rc=$ff_rc output=$ff_err"
fi

# The dry-run output must name the target commit B, proving it fetched and resolved origin/main.
if [[ "$ff_err" == *"$COMMIT_B"* ]]; then
    ok "fetch-before-resolve: output names origin/main commit (B), not stale local HEAD (A)"
else
    bad "fetch-before-resolve: output names origin/main commit (B), not stale local HEAD (A)" \
        "COMMIT_B=$COMMIT_B not found in: $ff_err"
fi

# ==========================================================================
echo
echo "live-aeon guard — positive control: old promote.sh (pre-guard) proceeds with live aeons:"
# ==========================================================================
# Before asserting that the guard refuses, prove that a version WITHOUT the guard would
# NOT refuse — confirming the invariant is discriminating, not trivially satisfied.
#
# Build a stripped promote.sh by removing the live-aeon guard block (identified by its
# unique SPIRA_PROMOTE_FORCE sentinel).
AG_OLD="$TMP/ag-old-promote.sh"
sed '/^if \[ -z "\${SPIRA_PROMOTE_FORCE/,/^fi$/{ /^fi$/d; d }' "$HERE/promote.sh" > "$AG_OLD"
chmod +x "$AG_OLD"

# A split-checkout fixture where prod is already up-to-date: the old promote exits 0.
AG_DEV="$TMP/ag-dev"
AG_ORIGIN="$TMP/ag-origin.git"
AG_PROD="$TMP/ag-prod"
mkdir -p "$AG_DEV/spira"
git -C "$TMP" init -q ag-dev 2>/dev/null
git -C "$AG_DEV" config user.email t@t
git -C "$AG_DEV" config user.name test
git -C "$AG_DEV" commit --allow-empty -m "ag-init" -q
git clone --bare -q "$AG_DEV" "$AG_ORIGIN"
git -C "$AG_DEV" remote add origin "$AG_ORIGIN"
git -C "$AG_DEV" push -q origin main 2>/dev/null || git -C "$AG_DEV" push -q origin HEAD:main 2>/dev/null || true
git clone -q "$AG_ORIGIN" "$AG_PROD" 2>/dev/null
git -C "$AG_PROD" checkout --detach -q HEAD
ln -sf "$HERE/lib.sh"  "$AG_DEV/spira/lib.sh"
ln -sf "$HERE/conf.sh" "$AG_DEV/spira/conf.sh"
printf '# empty\n' > "$AG_DEV/spira/repo-map.example"
printf '# empty\n' > "$AG_DEV/spira/watchers"

# Mock systemctl that reports one live aeon for the prod instance.
AG_MOCK="$TMP/ag-mock"
mkdir -p "$AG_MOCK"
cat > "$AG_MOCK/systemctl" <<'AGMOCK'
#!/usr/bin/env bash
case "$*" in
    *list-units*spira-aeon*) printf 'spira-aeon-builder-1234-prod.service\n' ;;
esac
exit 0
AGMOCK
chmod +x "$AG_MOCK/systemctl"

ag_old_rc=0
env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$AG_DEV/spira" \
    SPIRA_REPO="$AG_DEV" \
    SPIRA_PROD="$AG_PROD/spira" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$AG_DEV/spira/watchers" \
    "SPIRA_PATH=$AG_MOCK" \
    SPIRA_SYSTEMCTL="$AG_MOCK/systemctl" \
    bash "$AG_OLD" origin/main 2>/dev/null || ag_old_rc=$?

if [ "$ag_old_rc" -eq 0 ]; then
    ok "positive control: pre-guard promote.sh exits 0 with live aeons (this is the bug)"
else
    bad "positive control: pre-guard promote.sh exits 0 with live aeons" \
        "expected rc=0 (no guard), got rc=$ag_old_rc"
fi

# ==========================================================================
echo
echo "live-aeon guard — fixed promote.sh refuses when aeons are running:"
# ==========================================================================
ag_out="" ag_rc=0
ag_out="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$AG_DEV/spira" \
    SPIRA_REPO="$AG_DEV" \
    SPIRA_PROD="$AG_PROD/spira" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$AG_DEV/spira/watchers" \
    "SPIRA_PATH=$AG_MOCK" \
    SPIRA_SYSTEMCTL="$AG_MOCK/systemctl" \
    bash "$HERE/promote.sh" origin/main 2>&1)" || ag_rc=$?

if [ "$ag_rc" -ne 0 ]; then
    ok "aeon guard: promote.sh exits non-zero with live aeons (rc=$ag_rc)"
else
    bad "aeon guard: promote.sh exits non-zero with live aeons" "exited 0 — guard did not fire"
fi
want "aeon guard: names the live aeon unit" "spira-aeon-builder-1234" "$ag_out"
want "aeon guard: mentions SPIRA_PROMOTE_FORCE override" "SPIRA_PROMOTE_FORCE" "$ag_out"

# ==========================================================================
echo
echo "live-aeon guard — SPIRA_PROMOTE_FORCE=1 bypasses the guard:"
# ==========================================================================
ag_force_out="" ag_force_rc=0
ag_force_out="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$AG_DEV/spira" \
    SPIRA_REPO="$AG_DEV" \
    SPIRA_PROD="$AG_PROD/spira" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$AG_DEV/spira/watchers" \
    "SPIRA_PATH=$AG_MOCK" \
    SPIRA_SYSTEMCTL="$AG_MOCK/systemctl" \
    SPIRA_PROMOTE_FORCE=1 \
    bash "$HERE/promote.sh" origin/main 2>&1)" || ag_force_rc=$?

if [ "$ag_force_rc" -eq 0 ]; then
    ok "aeon guard: SPIRA_PROMOTE_FORCE=1 bypasses the guard (rc=0)"
else
    bad "aeon guard: SPIRA_PROMOTE_FORCE=1 bypasses the guard" \
        "still refused (rc=$ag_force_rc): $ag_force_out"
fi

# ==========================================================================
echo
echo "live-aeon guard — no aeons running: promote.sh proceeds normally:"
# ==========================================================================
# Mock systemctl that reports no aeons.
AG_QUIET="$TMP/ag-quiet"
mkdir -p "$AG_QUIET"
cat > "$AG_QUIET/systemctl" <<'QUIETMOCK'
#!/usr/bin/env bash
exit 0
QUIETMOCK
chmod +x "$AG_QUIET/systemctl"

ag_none_out="" ag_none_rc=0
ag_none_out="$(env -i PATH="$PATH" HOME="$TMP/home" \
    SPIRA_HOME="$AG_DEV/spira" \
    SPIRA_REPO="$AG_DEV" \
    SPIRA_PROD="$AG_PROD/spira" \
    SPIRA_RUN="$TMP/run" \
    SPIRA_DB="$TMP/db" \
    SPIRA_CONF=/nonexistent \
    SPIRA_WATCHERS="$AG_DEV/spira/watchers" \
    "SPIRA_PATH=$AG_QUIET" \
    SPIRA_SYSTEMCTL="$AG_QUIET/systemctl" \
    bash "$HERE/promote.sh" origin/main 2>&1)" || ag_none_rc=$?

if [ "$ag_none_rc" -eq 0 ]; then
    ok "aeon guard: no aeons → promote proceeds (rc=0)"
else
    bad "aeon guard: no aeons → promote proceeds" \
        "exited $ag_none_rc — guard fired spuriously: $ag_none_out"
fi
if [[ "$ag_none_out" != *"refusing"* ]]; then
    ok "aeon guard: no 'refusing' in output when no aeons"
else
    bad "aeon guard: no 'refusing' in output when no aeons" "found 'refusing' in: $ag_none_out"
fi

# ==========================================================================
echo
echo "summary"
# ==========================================================================
printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
