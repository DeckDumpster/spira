#!/usr/bin/env bash
#
# test-install-hooks-artifact.sh — install.sh phase 5 (git hooks) on a release
# tarball install vs. a git checkout.
#
#   ./test-install-hooks-artifact.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. ARTIFACT: SPIRA_REPO has no .git (an unpacked release tarball) → phase 5
#    skips with a one-line notice, and exclude.sh is never invoked (positive
#    control: a poison exclude.sh would mark a file if it ran).
# 2. GIT CHECKOUT: SPIRA_REPO is a git repo → exclude.sh still runs, and a
#    failure (no armable pre-commit hook) still fails the phase loudly —
#    install.sh exits 2, naming "exclude.sh install failed".
#
# Before sp-nakod, exclude.sh install was called unconditionally, so case 1
# failed every artifact install with "ROOT is not a git repository" / "core
# .hooksPath not set" — this is the install-side sibling of GitHub #315 /
# sp-bwaxb, which removed the same unconditional assumption from doctor.sh.
#
# covers: install.sh spira/exclude.sh spira/ctrl.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REAL_REPO="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"
is2()     { [ "$2" = 2 ] && ok "$1" || bad "$1" "wanted exit 2, got $2"; }

echo "test-install-hooks-artifact.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# Fixture layout — same pattern as test-install-conflicts.sh / test-install-
# prod-checkout.sh: a harness tree of stubs for everything install.sh calls
# except phase 5, which is the subject under test.
# ---------------------------------------------------------------------------
FIXTURE="$TMP/harness"
SPIRA_DIR="$FIXTURE/spira"
SYSTEMD_DIR="$FIXTURE/systemd"
COCKPIT_DIR="$FIXTURE/cockpit"
mkdir -p "$SPIRA_DIR" "$COCKPIT_DIR" "$SYSTEMD_DIR"

for f in "$HERE/../systemd/"*.service "$HERE/../systemd/"*.timer "$HERE/../systemd/"*.yaml; do
    [ -e "$f" ] || continue
    ln -s "$f" "$SYSTEMD_DIR/$(basename "$f")" 2>/dev/null || true
done
ln -s "$HERE/../systemd/install.sh" "$SYSTEMD_DIR/install.sh"
ln -s "$HERE/../systemd/units.sh"   "$SYSTEMD_DIR/units.sh"

ln -s "$HERE/conf.sh"         "$SPIRA_DIR/conf.sh"
ln -s "$HERE/lib.sh"          "$SPIRA_DIR/lib.sh"
ln -s "$HERE/suite-covers.sh" "$SPIRA_DIR/suite-covers.sh"
ln -s "$HERE/watchd.sh"       "$SPIRA_DIR/watchd.sh"

# ctrl.sh: report every unit as suspended, so phase 4's ExecStart-is-executable check
# (which the built Rust/Python binaries this fixture never builds would otherwise fail)
# is skipped for all of them. Phase 4's own correctness is covered elsewhere (e.g.
# test-units-lint.sh); this fixture only needs it to succeed so execution reaches phase 5.
# install.sh now sources this under CTRL_LIB=1 (sp-rnps9) instead of running it as a CLI
# per unit, so the stub must define the two functions that contract requires.
cat > "$SPIRA_DIR/ctrl.sh" <<'EOF'
#!/usr/bin/env bash
ctrl_load_suspended() { local -n _a="$1"; _a=(); }
ctrl_is_suspended() { printf 'stub\n'; return 0; }
if [ "${CTRL_LIB:-0}" != "1" ]; then
    [ "${1:-}" = check ] && exit 0
    exit 1
fi
EOF
chmod +x "$SPIRA_DIR/ctrl.sh"
printf '# empty\n' > "$SPIRA_DIR/watchers"
printf '# empty\n' > "$SPIRA_DIR/repo-map.example"
mkdir -p "$SPIRA_DIR/statutes"

cat > "$SPIRA_DIR/doctor.sh" <<'EOF'
#!/usr/bin/env bash
echo "spira doctor"
echo "  ok    stub — all checks passed"
exit 0
EOF
chmod +x "$SPIRA_DIR/doctor.sh"

cat > "$SPIRA_DIR/configure.sh" <<'EOF'
#!/usr/bin/env bash
_out="${XDG_CONFIG_HOME:-$HOME/.config}/spira/spira.conf"
if [ -f "$_out" ]; then exit 1; fi
mkdir -p "$(dirname "$_out")"
printf 'SPIRA_PROD = %s\n' "${CONFIGURE_PROD:-/nonexistent}" > "$_out"
EOF
chmod +x "$SPIRA_DIR/configure.sh"

cat > "$SPIRA_DIR/build.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$SPIRA_DIR/build.sh"

cat > "$SPIRA_DIR/seed.sh" <<'EOF'
#!/usr/bin/env bash
printf 'seed: stub\n'
exit 0
EOF
chmod +x "$SPIRA_DIR/seed.sh"

cat > "$SPIRA_DIR/ready.sh" <<'EOF'
#!/usr/bin/env bash
printf 'spira ready\n  FAIL  stub — not wired in fixture\n'
exit 1
EOF
chmod +x "$SPIRA_DIR/ready.sh"

cat > "$COCKPIT_DIR/layout.sh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$COCKPIT_DIR/layout.sh"

# Generic no-op ExecStart targets — every script phase 4's unit render checks
# for executability (systemd/install.sh refuses to write a unit whose target
# is not +x). None of these run for real in this suite; they exist so phase 4
# succeeds and execution reaches phase 5, the subject under test.
for _s in aeon.sh archive.sh archivist.sh auron.sh broker.sh czar.sh \
          gate-check.sh gh-intake.sh groom-trigger.sh maechen-trigger.sh \
          loom.sh mail.sh pr-notify.sh sentinel.sh skew.sh spira-mail-deliver.sh \
          spira-verdict.sh suites.sh watch-refresh.sh watchtower.sh; do
    printf '#!/usr/bin/env bash\ntrue\n' > "$SPIRA_DIR/$_s"
    chmod +x "$SPIRA_DIR/$_s"
done
for _s in moot-sweep.sh verify-asks.sh; do
    printf '#!/usr/bin/env bash\ntrue\n' > "$COCKPIT_DIR/$_s"
    chmod +x "$COCKPIT_DIR/$_s"
done
for _s in beads-push.sh concierge.sh; do
    printf '#!/usr/bin/env bash\ntrue\n' > "$FIXTURE/$_s"
    chmod +x "$FIXTURE/$_s"
done
unset _s

# POISON EXCLUDE.SH — records every invocation. Case 1 asserts this file is
# never written: the positive control that proves the skip branch, not luck,
# kept exclude.sh from running (law-absence-needs-a-positive-control).
EXCLUDE_LOG="$TMP/exclude-invocations.log"
cat > "$SPIRA_DIR/exclude.sh" <<EOF
#!/usr/bin/env bash
printf 'invoked: %s\n' "\$*" >> "$EXCLUDE_LOG"
exit 1
EOF
chmod +x "$SPIRA_DIR/exclude.sh"

ln -s "$REAL_REPO/install.sh" "$FIXTURE/install.sh"

MOCK_BIN="$TMP/mock-bin"
mkdir -p "$MOCK_BIN"
MOCK_LOG="$TMP/mock.log"

cat > "$MOCK_BIN/systemctl" <<EOF
#!/usr/bin/env bash
printf 'systemctl %s\n' "\$*" >> "${MOCK_LOG}"
case "\$*" in
    *is-active*) echo "inactive" ;;
    *list-unit-files*spira-watch*) printf 'spira-watch@testview.service enabled\n' ;;
    *list-units*spira-watch*)  printf 'spira-watch@testview.service loaded active running\n' ;;
    *list-units*active*spira-aeon*) true ;;
    *list-timers*) true ;;
esac
exit 0
EOF
chmod +x "$MOCK_BIN/systemctl"

cat > "$MOCK_BIN/loginctl" <<'EOF'
#!/usr/bin/env bash
case "$*" in *show-user*Linger*) echo "Linger=no" ;; esac
exit 0
EOF
chmod +x "$MOCK_BIN/loginctl"

cat > "$MOCK_BIN/tmux" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$MOCK_BIN/tmux"

cat > "$MOCK_BIN/dolt" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$MOCK_BIN/dolt"

cat > "$MOCK_BIN/bd" <<'EOF'
#!/usr/bin/env bash
case "$*" in
    *init*)     exit 0 ;;
    *list*)     printf '[]\n' ;;
    *memories*) printf '{}\n' ;;
    *)          exit 0 ;;
esac
EOF
chmod +x "$MOCK_BIN/bd"

FAKE_HOME="$TMP/home"
FAKE_UNITDIR="$FAKE_HOME/.config/systemd/user"
FAKE_RUN="$TMP/run"
FAKE_DB="$TMP/db"
mkdir -p "$FAKE_HOME" "$FAKE_UNITDIR" "$FAKE_RUN" "$FAKE_DB"

# Pre-seed FAKE_UNITDIR so phase 4's --diff / write step has something to
# compare and does not report every unit as a fresh change on every run.
_rendered="$(env -i \
    "PATH=$MOCK_BIN:$PATH" \
    "HOME=$FAKE_HOME" \
    SPIRA_CONF=/nonexistent \
    "SPIRA_PATH=$MOCK_BIN" \
    "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
    SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
    "SPIRA_RUN=$FAKE_RUN" \
    "SPIRA_HOME=$SPIRA_DIR" \
    "SPIRA_PROD=$SPIRA_DIR" \
    "SPIRA_REPO=$SPIRA_DIR" \
    "SPIRA_COCKPIT=$COCKPIT_DIR" \
    "MOCK_LOG=$MOCK_LOG" \
    SPIRA_INSTALL_FORCE=1 \
    "SPIRA_BD=$MOCK_BIN/bd" \
    bash "$SYSTEMD_DIR/install.sh" prod --render 2>/dev/null)"
_render_rc=$?
if [ "$_render_rc" = 0 ]; then
    _cur=""
    while IFS= read -r _line; do
        if [[ "$_line" =~ ^=====\ (.+)\ =====$ ]]; then
            _cur="${BASH_REMATCH[1]}"; > "$FAKE_UNITDIR/$_cur"
        elif [ -n "$_cur" ]; then
            printf '%s\n' "$_line" >> "$FAKE_UNITDIR/$_cur"
        fi
    done <<< "$_rendered"
    unset _cur _line
fi
unset _rendered _render_rc

run_install() {   # run_install <SPIRA_REPO> -- extra env assignments...
    local repo="$1"; shift
    [ "${1:-}" = "--" ] && shift
    > "$MOCK_LOG"
    env -i \
        "PATH=$MOCK_BIN:$PATH" \
        "HOME=$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        "SPIRA_PATH=$MOCK_BIN" \
        "SPIRA_WATCHERS=$SPIRA_DIR/watchers" \
        SPIRA_DOLT_DATA= SPIRA_TESTDB_DATA= \
        "SPIRA_RUN=$FAKE_RUN" \
        "SPIRA_HOME=$SPIRA_DIR" \
        "SPIRA_PROD=$SPIRA_DIR" \
        "SPIRA_DB=$FAKE_DB" \
        "SPIRA_REPO=$repo" \
        "SPIRA_COCKPIT=$COCKPIT_DIR" \
        "MOCK_LOG=$MOCK_LOG" \
        SPIRA_INSTALL_FORCE=1 \
        SPIRA_INSTALL_CONFLICT_CONSIDERED=1 \
        "SPIRA_BD=$MOCK_BIN/bd" \
        "$@" \
        bash "$FIXTURE/install.sh" prod --no-session-hook 2>&1
}

# ===========================================================================
echo
echo "CASE 1: artifact install (no .git) — phase 5 skips, exclude.sh not called"
# ===========================================================================
ARTIFACT_REPO="$TMP/artifact-repo"
mkdir -p "$ARTIFACT_REPO"

> "$EXCLUDE_LOG"
_art_out="$(run_install "$ARTIFACT_REPO")"

want   "artifact: phase 5 names the skip"          "no commit hooks to arm" "$_art_out"
want   "artifact: phase 5 names the path"           "$ARTIFACT_REPO"         "$_art_out"
nowant "artifact: does not report a hooks failure"  "phase hooks failed"     "$_art_out"
[ ! -s "$EXCLUDE_LOG" ] \
    && ok  "artifact: exclude.sh was never invoked (positive control)" \
    || bad "artifact: exclude.sh was never invoked" "log: $(cat "$EXCLUDE_LOG")"

# ===========================================================================
echo
echo "CASE 2: git checkout — phase 5 still runs exclude.sh, still fails loudly"
# ===========================================================================
# A git repo whose pre-commit hook is not (yet) armable — exclude.sh's real
# failure mode when core.hooksPath cannot be set (spira/exclude.sh install,
# "no executable hook at .../hooks/pre-commit").
GIT_REPO="$TMP/git-repo"
mkdir -p "$GIT_REPO/spira"
git init -q "$GIT_REPO" 2>/dev/null
git -C "$GIT_REPO" config user.email t@t
git -C "$GIT_REPO" config user.name test
# The harness signature (boundary + gate.sh + lib.sh together) one level under
# root, so exclude.sh finds a real harness scope and reaches the hooks check
# rather than exiting early with "no harness tree here".
for _sig in boundary gate.sh lib.sh; do : > "$GIT_REPO/spira/$_sig"; done
unset _sig
git -C "$GIT_REPO" add -A >/dev/null 2>&1
git -C "$GIT_REPO" commit -q -m seed >/dev/null 2>&1

# Use the real exclude.sh here (not the poison stub) so the failure is genuine,
# not staged — swap SPIRA_HOME's exclude.sh out for the fixture's real one.
rm -f "$SPIRA_DIR/exclude.sh"
ln -s "$HERE/exclude.sh" "$SPIRA_DIR/exclude.sh"

_git_out="$(run_install "$GIT_REPO")"
_git_rc=$?

is2  "git-checkout: install.sh exits 2 at the hooks phase" "$_git_rc"
want "git-checkout: names the hooks-phase failure" \
    "phase hooks failed" "$_git_out"
want "git-checkout: names exclude.sh install failed" \
    "exclude.sh install failed" "$_git_out"
nowant "git-checkout: does not take the artifact skip path" \
    "no commit hooks to arm" "$_git_out"

# ===========================================================================
echo
tl_summary
