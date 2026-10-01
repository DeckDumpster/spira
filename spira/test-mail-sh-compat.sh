#!/usr/bin/env bash
#
# test-mail-sh-compat.sh — build-tarball.sh ships a mail.sh compat symlink to the real
# `mail` binary, both in bin/ and at $SPIRA_HOME/mail.sh (spira/mail.sh), so callers
# outside this tree that were never repointed keep working: the Concierge persona text
# ("$SPIRA_HOME/mail.sh send operator ..."), brain's escalation-hook.sh (pattern-matches
# "mail.sh ... --kind question|suit" verbatim), and an operator's own aerc config (outside
# version control) that may still say "outgoing = <release>/spira/mail.sh" (sp-ooh1k).
#
# CASES
#   1. POSITIVE CONTROL: before symlinking, bin/mail.sh and spira/mail.sh do not exist —
#      proves the later assertions test something this build actually did, not a fixture
#      artifact.
#   2. bin/mail.sh is a symlink to mail; spira/mail.sh is a symlink resolving to bin/mail.
#   3. `mail.sh list <box>` (either location) behaves exactly like `mail list <box>` —
#      same stdout, same exit code — because it is the identical binary.
#   4. A real, tracked spira/mail.sh is never clobbered by the compat symlink (the
#      `[ ! -e ... ]` guard in build-tarball.sh).
#
# tier: T1
# covers: spira/build-tarball.sh mail/src/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-mail-sh-compat.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

run_build() {
    env -i PATH="$PATH" HOME="$TMP/home" GIT_CONFIG_GLOBAL=/dev/null \
        GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t \
        bash "$HERE/build-tarball.sh" "$@" 2>&1
}
mkdir -p "$TMP/home"

# ---------------------------------------------------------------------------
# GIT FIXTURE — a spira/ directory must exist and be tracked for spira/mail.sh to land in.
# ---------------------------------------------------------------------------
ORIGIN="$TMP/origin.git"; REPO="$TMP/repo"
git init -q --bare -b main "$ORIGIN"
git clone -q "$ORIGIN" "$REPO" 2>/dev/null
git -C "$REPO" config user.email t@t
git -C "$REPO" config user.name t
mkdir -p "$REPO/spira"
printf 'key=val\n' > "$REPO/spira/conf.sh"
# sp-nhf25: the compat table build-tarball.sh reads is spira/deps.toml's [[compat]], not a
# hand-written list in this script — so the fixture must carry one, the same way a real
# checkout does, or the build step has nothing to symlink and every case below goes dark.
printf '[[compat]]\nname = "mail"\nalias = "mail.sh"\n' > "$REPO/spira/deps.toml"
git -C "$REPO" add .
git -C "$REPO" commit -qm "fixture: initial"
git -C "$REPO" push -q origin main 2>/dev/null

# ---------------------------------------------------------------------------
# STUB "mail" BINARY — enough of `list` to prove the symlinks forward argv identically.
# ---------------------------------------------------------------------------
mkdir -p "$TMP/bins"
MAIL_BIN="$TMP/bins/mail"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "MAIL_STUB:$*"\n' > "$MAIL_BIN"
chmod +x "$MAIL_BIN"

# ============================================================================
echo
echo "1. POSITIVE CONTROL — the source fixture carries no mail.sh of its own"
# ============================================================================
# Proves what follows is the build's own symlink step, not a fixture artifact that would
# have made bin/mail.sh and spira/mail.sh appear regardless.
if [ -e "$REPO/spira/mail.sh" ]; then
    bad "case 1: the fixture repo starts with no spira/mail.sh" "already present"
else
    ok "case 1: the fixture repo starts with no spira/mail.sh"
fi

OUT1="$TMP/out1"
build1_rc=0
build1_out="$(run_build build --output "$OUT1" --bin-dir "$TMP/bins" HEAD "$REPO" 2>&1)" || build1_rc=$?
is "build exits 0" "0" "$build1_rc"

TARBALL1="$(find "$OUT1" -name 'spira-*.tar.gz' | head -1)"
[ -n "$TARBALL1" ] || { bad "a tarball was produced" "none found: $build1_out"; tl_summary; exit 1; }
UNPACK1="$TMP/unpack1"; mkdir -p "$UNPACK1"
tar -xzf "$TARBALL1" -C "$UNPACK1"
STAGE1="$(find "$UNPACK1" -maxdepth 1 -mindepth 1 -type d | head -1)"

# ============================================================================
echo
echo "2. bin/mail.sh and spira/mail.sh are compat symlinks to the real mail binary"
# ============================================================================
[ -L "$STAGE1/bin/mail.sh" ] && ok "bin/mail.sh is a symlink" \
                              || bad "bin/mail.sh is a symlink" "$([ -e "$STAGE1/bin/mail.sh" ] && echo 'exists but not a symlink' || echo 'missing')"
is "bin/mail.sh resolves to the real mail binary" \
    "$(readlink -f "$STAGE1/bin/mail")" "$(readlink -f "$STAGE1/bin/mail.sh")"

[ -L "$STAGE1/spira/mail.sh" ] && ok "spira/mail.sh is a symlink" \
                                || bad "spira/mail.sh is a symlink" "$([ -e "$STAGE1/spira/mail.sh" ] && echo 'exists but not a symlink' || echo 'missing')"
is "spira/mail.sh (\$SPIRA_HOME/mail.sh) resolves to the real mail binary" \
    "$(readlink -f "$STAGE1/bin/mail")" "$(readlink -f "$STAGE1/spira/mail.sh")"

# ============================================================================
echo
echo "3. mail.sh list <box>, from either compat path, behaves exactly like mail list <box>"
# ============================================================================
real_out="$("$STAGE1/bin/mail" list operator)"; real_rc=$?
bin_compat_out="$("$STAGE1/bin/mail.sh" list operator)"; bin_compat_rc=$?
home_compat_out="$("$STAGE1/spira/mail.sh" list operator)"; home_compat_rc=$?

is "bin/mail.sh: same exit code as mail"    "$real_rc" "$bin_compat_rc"
is "bin/mail.sh: same stdout as mail"       "$real_out" "$bin_compat_out"
is "spira/mail.sh: same exit code as mail"  "$real_rc" "$home_compat_rc"
is "spira/mail.sh: same stdout as mail"     "$real_out" "$home_compat_out"
want "the stub actually saw the forwarded argv (positive control)" "list operator" "$real_out"

# ============================================================================
echo
echo "4. a real, tracked spira/mail.sh is never clobbered by the compat symlink"
# ============================================================================
REPO2="$TMP/repo2"
git clone -q "$ORIGIN" "$REPO2" 2>/dev/null
git -C "$REPO2" config user.email t@t
git -C "$REPO2" config user.name t
printf '#!/usr/bin/env bash\necho REAL_TRACKED_MAIL_SH\n' > "$REPO2/spira/mail.sh"
chmod +x "$REPO2/spira/mail.sh"
git -C "$REPO2" add .
git -C "$REPO2" commit -qm "fixture: a real, tracked spira/mail.sh appears"

OUT2="$TMP/out2"
build2_rc=0
run_build build --output "$OUT2" --bin-dir "$TMP/bins" HEAD "$REPO2" >/dev/null 2>&1 || build2_rc=$?
is "build (case 4) exits 0" "0" "$build2_rc"

TARBALL2="$(find "$OUT2" -name 'spira-*.tar.gz' | head -1)"
UNPACK2="$TMP/unpack2"; mkdir -p "$UNPACK2"
tar -xzf "$TARBALL2" -C "$UNPACK2"
STAGE2="$(find "$UNPACK2" -maxdepth 1 -mindepth 1 -type d | head -1)"

if [ -L "$STAGE2/spira/mail.sh" ]; then
    bad "spira/mail.sh is not a symlink when a real one was tracked" "it was replaced with a symlink"
else
    ok "spira/mail.sh is not a symlink when a real one was tracked"
fi
want "the real, tracked spira/mail.sh survives untouched" "REAL_TRACKED_MAIL_SH" "$(cat "$STAGE2/spira/mail.sh")"

tl_summary
