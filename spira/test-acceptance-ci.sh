#!/usr/bin/env bash
# covers: spira/acceptance-ci.sh .github/workflows/acceptance.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

pass=0; fail=0
ok()      { pass=$((pass+1)); printf '  ok    %s\n' "$1"; }
bad()     { fail=$((fail+1)); printf '  FAIL  %s: %s\n' "$1" "${2:-}"; }
iszero()  { [ "$2" = 0 ] && ok "$1" || bad "$1" "exit $2"; }
notzero() { [ "$2" != 0 ] && ok "$1" || bad "$1" "wanted non-zero exit"; }
want()    { [[ "$3" == *"$2"* ]] && ok "$1" || bad "$1" "wanted [$2] in [$3]"; }

echo "test-acceptance-ci.sh"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

# Stub acceptance-run.sh: records args, exits with $STUB_RC.
# STUB_RC is inherited from the caller's environment.
STUB="$SCRATCH/stub-acceptance-run.sh"
cat > "$STUB" <<STUB_BODY
#!/usr/bin/env bash
printf '%s\n' "\$*" > "$SCRATCH/stub-args"
exit "\${STUB_RC:-0}"
STUB_BODY
chmod +x "$STUB"

# --- positive control: --initial-branch works; plain init gives a different branch ---
# Proves the mechanism before trusting the silence in the main case.
git init --bare --initial-branch=master "$SCRATCH/ctrl-master.git" >/dev/null 2>&1
_ctrl="$(git -C "$SCRATCH/ctrl-master.git" symbolic-ref HEAD 2>/dev/null || true)"
want "positive-control: --initial-branch=master gives refs/heads/master" \
    "refs/heads/master" "$_ctrl"

# --- positive control: exit code propagation (stub exits 1 → script exits 1) ---
CI_HOME="$SCRATCH/home1"
mkdir -p "$CI_HOME"
git config --file "$CI_HOME/.gitconfig" user.email "t@spira" 2>/dev/null || true
git config --file "$CI_HOME/.gitconfig" user.name "T" 2>/dev/null || true

_rc=0
HOME="$CI_HOME" STUB_RC=1 \
    SPIRA_ACCEPTANCE_RUN="$STUB" \
    XDG_CONFIG_HOME="$CI_HOME/.config" \
    bash "$HERE/acceptance-ci.sh" "any-tag" --bd-db "$SCRATCH/bd" \
    >/dev/null 2>&1 || _rc=$?
notzero "positive-control: exits non-zero when acceptance-run exits 1" "$_rc"

# --- normal run: stub exits 0 → script exits 0; scratch repo uses main ---
CI_HOME="$SCRATCH/home2"
mkdir -p "$CI_HOME"
git config --file "$CI_HOME/.gitconfig" user.email "t@spira" 2>/dev/null || true
git config --file "$CI_HOME/.gitconfig" user.name "T" 2>/dev/null || true
rm -f "$SCRATCH/stub-args"

_rc=0
HOME="$CI_HOME" STUB_RC=0 \
    SPIRA_ACCEPTANCE_RUN="$STUB" \
    XDG_CONFIG_HOME="$CI_HOME/.config" \
    bash "$HERE/acceptance-ci.sh" "test-tag" --bd-db "$SCRATCH/bd" \
    >/dev/null 2>&1 || _rc=$?
iszero "acceptance-ci exits 0 when acceptance-run exits 0" "$_rc"

# --- stub was reached (script called acceptance-run.sh) ---
[ -f "$SCRATCH/stub-args" ] \
    && ok "acceptance-run.sh was called" \
    || bad "acceptance-run.sh was called" "sentinel file not written"

# --- stub received the tag ---
_args="$(cat "$SCRATCH/stub-args" 2>/dev/null || true)"
want "acceptance-run called with the tag" "test-tag" "$_args"

# --- bare repo created with main as initial branch ---
_head="$(git -C "$CI_HOME/scratch-repo.git" symbolic-ref HEAD 2>/dev/null || true)"
want "git init uses --initial-branch=main" "refs/heads/main" "$_head"

# --- clone is on main ---
_branch="$(git -C "$CI_HOME/scratch-repo" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
want "scratch repo clone is on main" "main" "$_branch"

# --- repo-map written ---
_rmap="$CI_HOME/.config/spira/repo-map"
[ -f "$_rmap" ] && ok "repo-map written" || bad "repo-map written" "not found"

# --- PATH injection: real acceptance-run.sh prerequisite stage ---
# acceptance-ci.sh must export $HOME/.local/bin onto PATH before calling
# acceptance-run.sh. Proven here against the real prerequisite check.
#
# /usr/local/bin (where the container's bd lives) is excluded from PATH so
# the only way acceptance-run.sh can find bd is via the injection.
_SYSPATH="/usr/local/sbin:/usr/sbin:/sbin:/usr/bin:/bin"

# pass: stub bd in .local/bin; after acceptance-ci injection the prereq passes
CI_HOME3="$SCRATCH/home3"
mkdir -p "$CI_HOME3/.local/bin"
git config --file "$CI_HOME3/.gitconfig" user.email "t@spira" 2>/dev/null || true
git config --file "$CI_HOME3/.gitconfig" user.name "T" 2>/dev/null || true
for _t in bd dolt gh; do
    printf '#!/bin/sh\nexit 0\n' > "$CI_HOME3/.local/bin/$_t"
    chmod +x "$CI_HOME3/.local/bin/$_t"
done
HOME="$CI_HOME3" PATH="$_SYSPATH" XDG_CONFIG_HOME="$CI_HOME3/.config" \
    bash "$HERE/acceptance-ci.sh" "no-such-tag-$$" --bd-db "$SCRATCH/bd3" \
    >"$SCRATCH/out3" 2>&1 || true
want "prereq-pass: bd found on PATH via acceptance-ci injection" \
    "ok    prereq: bd on PATH" "$(cat "$SCRATCH/out3")"

# pair: bd absent from .local/bin; prereq check fails on that line
CI_HOME4="$SCRATCH/home4"
mkdir -p "$CI_HOME4/.local/bin"
git config --file "$CI_HOME4/.gitconfig" user.email "t@spira" 2>/dev/null || true
git config --file "$CI_HOME4/.gitconfig" user.name "T" 2>/dev/null || true
for _t in dolt gh; do  # bd deliberately absent
    printf '#!/bin/sh\nexit 0\n' > "$CI_HOME4/.local/bin/$_t"
    chmod +x "$CI_HOME4/.local/bin/$_t"
done
HOME="$CI_HOME4" PATH="$_SYSPATH" XDG_CONFIG_HOME="$CI_HOME4/.config" \
    bash "$HERE/acceptance-ci.sh" "no-such-tag-$$" --bd-db "$SCRATCH/bd4" \
    >"$SCRATCH/out4" 2>&1 || true
want "prereq-fail: bd absent → prereq check names it" \
    "FAIL  prereq: bd on PATH" "$(cat "$SCRATCH/out4")"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
