#!/usr/bin/env bash
#
# test-land-build-ensure.sh — land-build-ensure.sh must rebuild a cargo binary
# that is missing while its systemd unit is enabled, for every binary it names.
#
# sp-2mn5v: spira-czar-pass-prod.service exited 2 for four days because the
# czar-pass binary was never built here — no release installed, and nothing
# ran cargo build in the production checkout. land-build-ensure.sh already
# carries the czar-pass entry that heals this (sp-i84vy), but nothing tested
# that the entry works, or would keep working if a future edit dropped it.
#
# POSITIVE CONTROL (law-absence-needs-a-positive-control): for each binary,
# first prove build.sh is NOT run when the unit is disabled (or the binary is
# present), then prove it IS run when the binary is missing and the unit is
# enabled. A rebuild trigger that fires unconditionally is not a trigger.
#
# host-reason: for the binary-absent pairs, SPIRA_REPO is a plain temp dir, not
# a git checkout, so the script's git calls fail closed and take the "binary
# missing" branch only — no real systemd, cargo build, or database required.
#
# gap G10: the OTHER trigger — cargo source changed, read from the reflog one
# step behind the checkout's current branch (`git rev-parse "$branch@{1}"`) —
# has no case above, because it needs a real git repo to move. A real one is
# built below: skew.sh's own refresh() advances the checkout with `git reset
# --mixed`, which is exactly what writes that reflog entry, so this drives the
# same primitive rather than a hand-written model of it.
#
# covers: spira/land-build-ensure.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"

pass=0; fail=0
ok()  { pass=$((pass+1)); printf '  ok   — %s\n' "$1"; }
bad() { fail=$((fail+1)); printf '  FAIL — %s\n' "$1"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

REPO="$TMP/repo"
mkdir -p "$REPO/spira"

BUILD_LOG="$TMP/build.log"
cat > "$REPO/spira/build.sh" <<EOF
#!/usr/bin/env bash
printf 'built\n' >> "$BUILD_LOG"
EOF
chmod +x "$REPO/spira/build.sh"

# A mock systemctl: "is-enabled <unit>" exits 0 only for the unit named in
# ENABLED_UNIT, matching the pattern SPIRA_SYSTEMCTL --user is-enabled expects.
MOCK_SC="$TMP/mock-sc"
cat > "$MOCK_SC" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "--user" ] && [ "$2" = "is-enabled" ]; then
    [ "$3" = "${ENABLED_UNIT:-}" ]
    exit $?
fi
exit 1
EOF
chmod +x "$MOCK_SC"

# A present binary — any executable file — so pairs not under test never
# contribute to _be_missing.
PRESENT_BIN="$TMP/present-bin"
printf '#!/usr/bin/env bash\n' > "$PRESENT_BIN"
chmod +x "$PRESENT_BIN"

ABSENT_BIN="$TMP/no-such-dir/absent-bin"

run_ensure() {
    rm -f "$BUILD_LOG"
    ( cd "$TMP" && env -i \
        PATH="$PATH" \
        SPIRA_REPO="$REPO" \
        SPIRA_SYSTEMCTL="$MOCK_SC" \
        SPIRA_INSTANCE="testinst" \
        ENABLED_UNIT="${ENABLED_UNIT:-}" \
        SPIRA_LOOM_BIN="${SPIRA_LOOM_BIN:-$PRESENT_BIN}" \
        SPIRA_BROKER_BIN="${SPIRA_BROKER_BIN:-$PRESENT_BIN}" \
        SPIRA_PANEL="${SPIRA_PANEL:-$PRESENT_BIN}" \
        SPIRA_CZAR_PASS_BIN="${SPIRA_CZAR_PASS_BIN:-$PRESENT_BIN}" \
        bash "$HERE/land-build-ensure.sh" >"$TMP/out.log" 2>&1 )
}

built() { [ -f "$BUILD_LOG" ]; }

# Four (env var, unit base, unit type) triples, matching land-build-ensure.sh's
# own pairs list exactly — a dropped entry here is exactly the regression this
# suite exists to catch.
TRIPLES="SPIRA_LOOM_BIN:loom:service SPIRA_BROKER_BIN:broker:timer SPIRA_PANEL:cockpit:service SPIRA_CZAR_PASS_BIN:czar-pass:timer"

for triple in $TRIPLES; do
    var="${triple%%:*}"
    rest="${triple#*:}"
    base="${rest%%:*}"
    typ="${rest#*:}"
    unit="spira-${base}-testinst.${typ}"

    # Positive control: binary missing, unit NOT enabled — no rebuild.
    unset ENABLED_UNIT
    eval "export $var=\"\$ABSENT_BIN\""
    run_ensure
    if built; then
        bad "$var: missing + unit disabled must not rebuild"
    else
        ok "$var: missing + unit disabled does not rebuild (positive control)"
    fi
    unset "$var"

    # The fence: binary missing, unit enabled — rebuild fires.
    export ENABLED_UNIT="$unit"
    eval "export $var=\"\$ABSENT_BIN\""
    run_ensure
    if built; then
        ok "$var: missing + unit ($unit) enabled triggers build.sh"
    else
        bad "$var: missing + unit ($unit) enabled must trigger build.sh"
    fi
    unset "$var" ENABLED_UNIT

    # Binary present, unit enabled — no rebuild needed.
    export ENABLED_UNIT="$unit"
    eval "export $var=\"\$PRESENT_BIN\""
    run_ensure
    if built; then
        bad "$var: present binary must not trigger a rebuild"
    else
        ok "$var: present binary does not trigger a rebuild"
    fi
    unset "$var" ENABLED_UNIT
done

# ============================================================================
# gap G10: the reflog-based source-changed trigger, over a real git repo.
# ============================================================================
GITREPO="$TMP/gitrepo"
mkdir -p "$GITREPO/spira"
git -C "$GITREPO" init -q -b maintrig
git -C "$GITREPO" config user.email t@t
git -C "$GITREPO" config user.name t
cp "$REPO/spira/build.sh" "$GITREPO/spira/build.sh"
printf 'seed\n' > "$GITREPO/seed.txt"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m seed

run_ensure_git() {
    rm -f "$BUILD_LOG"
    ( cd "$TMP" && env -i \
        PATH="$PATH" \
        SPIRA_REPO="$GITREPO" \
        SPIRA_SYSTEMCTL="$MOCK_SC" \
        SPIRA_INSTANCE="testinst" \
        SPIRA_LOOM_BIN="$PRESENT_BIN" \
        SPIRA_BROKER_BIN="$PRESENT_BIN" \
        SPIRA_PANEL="$PRESENT_BIN" \
        SPIRA_CZAR_PASS_BIN="$PRESENT_BIN" \
        bash "$HERE/land-build-ensure.sh" >"$TMP/out.log" 2>&1 )
}

# POSITIVE CONTROL: the checkout moves (a real reflog entry is written) but the
# diff between the reflog's previous position and HEAD touches no .rs file —
# proving this is a name-filtered diff, not "any movement rebuilds".
printf 'plain\n' > "$GITREPO/plain.txt"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m "non-rust change"
run_ensure_git
if built; then
    bad "a non-.rs commit must not trigger the reflog-based rebuild"
else
    ok "positive control: a non-.rs commit does not trigger the reflog-based rebuild (checkout still moved)"
fi

# THE TRIGGER: a commit that touches a .rs file fires build.sh, read from the
# branch's own reflog one step back — the same primitive skew.sh's refresh()
# advances the checkout with (git reset --mixed) rather than a hand-modeled diff.
mkdir -p "$GITREPO/loom/src"
printf 'fn main() {}\n' > "$GITREPO/loom/src/main.rs"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m "rust change"
run_ensure_git
if built; then
    ok "a .rs-touching commit triggers build.sh via the branch's own reflog"
else
    bad "a .rs-touching commit must trigger build.sh via the branch's own reflog"
fi

# A detached HEAD (no branch to read a reflog "one step back" from) never
# reaches the diff at all — symbolic-ref fails closed rather than guessing.
git -C "$GITREPO" checkout -q --detach HEAD
printf 'fn other() {}\n' >> "$GITREPO/loom/src/main.rs"
git -C "$GITREPO" add -A && git -C "$GITREPO" commit -q -m "rust change on a detached HEAD"
run_ensure_git
if built; then
    bad "a detached HEAD must not trigger the reflog-based rebuild"
else
    ok "a detached HEAD never reaches the reflog-based rebuild (symbolic-ref fails closed)"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
