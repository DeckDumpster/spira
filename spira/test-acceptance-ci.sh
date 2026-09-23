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

# --- scratch repo has git identity set (no home .gitconfig needed for git notes) ---
# On a runner with no global git config, git notes fails with "unable to auto-detect
# email address". acceptance-ci.sh must set user.email/name in the scratch repo's
# per-repo config so git notes in acceptance-run.sh works without a home identity.
CI_HOME5="$SCRATCH/home5"
mkdir -p "$CI_HOME5/.local/bin"
for _t in bd dolt gh; do
    printf '#!/bin/sh\nexit 0\n' > "$CI_HOME5/.local/bin/$_t"
    chmod +x "$CI_HOME5/.local/bin/$_t"
done
HOME="$CI_HOME5" STUB_RC=0 \
    SPIRA_ACCEPTANCE_RUN="$STUB" \
    XDG_CONFIG_HOME="$CI_HOME5/.config" \
    bash "$HERE/acceptance-ci.sh" "test-tag-$$" --bd-db "$SCRATCH/bd5" \
    >/dev/null 2>&1 || true
_git_email="$(git -C "$CI_HOME5/scratch-repo" config user.email 2>/dev/null || true)"
want "scratch-repo has user.email set for git notes" \
    "acceptance@spira.local" "$_git_email"

# --- XDG_RUNTIME_DIR and DBUS_SESSION_BUS_ADDRESS pass-through ---
# acceptance.yml's "Enable systemd user session" step writes both to $GITHUB_ENV;
# they must be present for install.sh and doctor.sh to reach the user manager.
_ACCYML="$HERE/../.github/workflows/acceptance.yml"
_yml="$(cat "$_ACCYML" 2>/dev/null || true)"
want "acceptance.yml: DBUS_SESSION_BUS_ADDRESS written to GITHUB_ENV" \
    "DBUS_SESSION_BUS_ADDRESS" "$_yml"
want "acceptance.yml: XDG_RUNTIME_DIR written to GITHUB_ENV" \
    "XDG_RUNTIME_DIR" "$_yml"

# Verify acceptance-ci.sh passes XDG_RUNTIME_DIR through to acceptance-run.sh.
CI_HOME6="$SCRATCH/home6"
mkdir -p "$CI_HOME6"
STUB6="$SCRATCH/stub6.sh"
cat > "$STUB6" <<STUB6_BODY
#!/usr/bin/env bash
printf '%s\n' "\${XDG_RUNTIME_DIR:-UNSET}" > "$SCRATCH/xdg-seen"
exit 0
STUB6_BODY
chmod +x "$STUB6"
XDG_RUNTIME_DIR="/run/user/1001" \
HOME="$CI_HOME6" \
XDG_CONFIG_HOME="$CI_HOME6/.config" \
SPIRA_ACCEPTANCE_RUN="$STUB6" \
    bash "$HERE/acceptance-ci.sh" "any-tag" --bd-db "$SCRATCH/bd6" \
    >/dev/null 2>&1 || true
_xdg_seen="$(cat "$SCRATCH/xdg-seen" 2>/dev/null || true)"
want "XDG_RUNTIME_DIR passes through to acceptance-run.sh" \
    "/run/user/1001" "$_xdg_seen"

# --- SPIRA_ACCEPTANCE_FORENSICS: acceptance-ci.sh sets the dir before calling
# acceptance-run.sh so the forensics survive the acceptance-run.sh TMP cleanup.
CI_HOME7="$SCRATCH/home7"
mkdir -p "$CI_HOME7"
STUB7="$SCRATCH/stub7.sh"
cat > "$STUB7" <<STUB7_BODY
#!/usr/bin/env bash
printf '%s\n' "\${SPIRA_ACCEPTANCE_FORENSICS:-UNSET}" > "$SCRATCH/forensics-seen"
exit 0
STUB7_BODY
chmod +x "$STUB7"
HOME="$CI_HOME7" STUB_RC=0 \
    SPIRA_ACCEPTANCE_RUN="$STUB7" \
    XDG_CONFIG_HOME="$CI_HOME7/.config" \
    bash "$HERE/acceptance-ci.sh" "any-tag" --bd-db "$SCRATCH/bd7" \
    >/dev/null 2>&1 || true

_fseen="$(cat "$SCRATCH/forensics-seen" 2>/dev/null || true)"
want "SPIRA_ACCEPTANCE_FORENSICS is set before calling acceptance-run.sh" \
    "$CI_HOME7/acceptance-forensics" "$_fseen"

# The forensics dir must be created (not just exported) before acceptance-run.sh runs.
[ -d "$CI_HOME7/acceptance-forensics" ] \
    && ok "acceptance-forensics dir created by acceptance-ci.sh" \
    || bad "acceptance-forensics dir created by acceptance-ci.sh" "directory not found"

# --- acceptance.yml: upload-artifact step present under if: always() ---
_yml="$(cat "$HERE/../.github/workflows/acceptance.yml" 2>/dev/null || true)"
want "acceptance.yml: upload-artifact step present" \
    "upload-artifact" "$_yml"
want "acceptance.yml: upload runs under if: always()" \
    "if: always()" "$_yml"
want "acceptance.yml: artifact named after tag" \
    "acceptance-forensics-" "$_yml"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
