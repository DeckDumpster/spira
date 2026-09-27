#!/usr/bin/env bash
#
# test-install-conf-seed.sh — systemd/install.sh's _seed_prod_instance, called directly.
#
# _seed_prod_instance writes SPIRA_INSTANCE=<instance> into a separate $SPIRA_PROD
# checkout's config so a non-prod sentinel's containment fence fires (see install.sh's own
# comment at the call site). Until sp-usxfl it hand-appended `SPIRA_INSTANCE=<inst>` lines to
# spira.conf; now it goes through conf.sh's spira_config_set_at, which always writes
# spira.toml — creating it first, via a full auto-convert, when the target root has only a
# legacy spira.conf or neither file yet. This suite's own PROPERTY 4 is the regression case:
# a root with only spira.conf must gain a spira.toml, not another line in the .conf.
#
# SPIRA_INSTALL_LIB=1 sourcing install.sh defines _seed_prod_instance (alongside
# _unit_action) and returns before conf.sh, unit rendering or the systemctl apply loop run —
# but _seed_prod_instance itself now calls conf.sh's own helpers, so each case here also
# sources a minimal conf.sh fixture (the same harness shape test-conf-writeback.sh and
# test-conf-toml.sh already build: no .git, so SPIRA_REPO derives to the fixture itself)
# in the SAME subshell, before calling the function.
#
#   ./test-install-conf-seed.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. SEED: neither file exists at the target root -> a spira.toml is created there with
#    instance set, and reports it. No spira.conf appears.
# 2. IDEMPOTENT: a second call reports nothing and leaves the toml unchanged.
# 3. EXISTING TOML KEYS PRESERVED: a sibling key already in the target's spira.toml survives.
# 4. LEGACY spira.conf AT THE TARGET: still gets a spira.toml (via auto-convert) with the
#    instance seeded — the spira.conf itself is left exactly as it was, never appended to.
#
# POSITIVE CONTROL: each property's fail-first case is checked before its happy-path case
# claims success — a call that silently did nothing would look "idempotent" or "preserved"
# too.
#
# tier: T1
# covers: systemd/install.sh spira/conf.sh UC-instance-lifecycle-29
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-install-conf-seed.sh"

CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
[ -n "$CARGO_BIN" ] || skip "cargo not found — spira-config binary cannot be built"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

SPIRA_CONFIG_BIN="$HERE/../target/release/spira-config"
if [ ! -x "$SPIRA_CONFIG_BIN" ]; then
    CARGO_TARGET_DIR="$TMP/cargo-target" "$CARGO_BIN" build --release \
        --manifest-path "$HERE/../spira-config/Cargo.toml" >/dev/null 2>&1
    SPIRA_CONFIG_BIN="$TMP/cargo-target/release/spira-config"
fi
[ -x "$SPIRA_CONFIG_BIN" ] || bail "spira-config binary not found/built at $SPIRA_CONFIG_BIN"

# A minimal harness tree, same shape test-conf-writeback.sh and test-conf-toml.sh already
# build: no .git, so SPIRA_REPO derives to the fixture itself rather than any real checkout.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"
FIXHOME="$TMP/home"; mkdir -p "$FIXHOME"

# seed <conf> <toml> <instance> -> stdout+stderr of one _seed_prod_instance call, run in a
# fresh subshell with conf.sh sourced (for the helpers) after install.sh (for the function).
seed() {
    local conf="$1" toml="$2" inst="$3"
    env -i PATH="$PATH" HOME="$FIXHOME" \
        SPIRA_CONFIG_BIN="$SPIRA_CONFIG_BIN" \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        SPIRA_CONF=/nonexistent SPIRA_TOML=/nonexistent \
        bash -c "
            SPIRA_INSTALL_LIB=1 . '$HERE/../systemd/install.sh'
            . '$HARNESS/spira/conf.sh'
            _seed_prod_instance '$conf' '$toml' '$inst'
        " 2>&1
}

# ==========================================================================
echo
echo "PROPERTY 1: SEED — neither file exists at the target root"
# ==========================================================================
ROOT1="$TMP/root1"; mkdir -p "$ROOT1"
CONF1="$ROOT1/spira.conf"; TOML1="$ROOT1/spira.toml"
INST1="test$$-a"

# FAIL-FIRST: before seeding, nothing exists.
if [ ! -e "$CONF1" ] && [ ! -e "$TOML1" ]; then
    ok "fail-first: neither spira.conf nor spira.toml exists yet at the target root"
else
    bad "fail-first: neither spira.conf nor spira.toml exists yet" "found one unexpectedly"
fi

out1="$(seed "$CONF1" "$TOML1" "$INST1")"
want "seed: reports the seeding" "seeded" "$out1"
if [ -f "$TOML1" ]; then
    ok "seed: spira.toml created at the target root"
else
    bad "seed: spira.toml created at the target root" "missing: $TOML1"
fi
got1="$("$SPIRA_CONFIG_BIN" get spira.instance "$TOML1" 2>/dev/null)"
is "seed: instance recorded in the new spira.toml" "$INST1" "$got1"
if [ ! -e "$CONF1" ]; then
    ok "seed: no spira.conf created beside it"
else
    bad "seed: no spira.conf created beside it" "found: $CONF1"
fi

# ==========================================================================
echo
echo "PROPERTY 2: IDEMPOTENT — a second call with the same instance is silent"
# ==========================================================================
out2="$(seed "$CONF1" "$TOML1" "$INST1")"
is "idempotent: second call reports nothing" "" "$out2"
got2="$("$SPIRA_CONFIG_BIN" get spira.instance "$TOML1" 2>/dev/null)"
is "idempotent: instance unchanged" "$INST1" "$got2"

# ==========================================================================
echo
echo "PROPERTY 3: EXISTING TOML KEYS PRESERVED — a sibling key survives the seed"
# ==========================================================================
ROOT3="$TMP/root3"; mkdir -p "$ROOT3"
CONF3="$ROOT3/spira.conf"; TOML3="$ROOT3/spira.toml"
INST3="test$$-b"
cat > "$TOML3" <<'EOF'
[spira]
max_aeons = 4
EOF

# FAIL-FIRST: the fixture toml has no instance key yet.
if [ "$("$SPIRA_CONFIG_BIN" get spira.instance "$TOML3" 2>/dev/null)" != "$INST3" ]; then
    ok "fail-first: fixture toml has no matching instance key yet"
else
    bad "fail-first: fixture toml has no matching instance key yet" "found unexpectedly"
fi

seed "$CONF3" "$TOML3" "$INST3" >/dev/null
want "preserved: sibling key survives" "max_aeons = 4" "$(cat "$TOML3")"
got3="$("$SPIRA_CONFIG_BIN" get spira.instance "$TOML3" 2>/dev/null)"
is "preserved: instance recorded alongside it" "$INST3" "$got3"

# ==========================================================================
echo
echo "PROPERTY 4: LEGACY spira.conf AT THE TARGET — gets a spira.toml, never appended to"
# ==========================================================================
ROOT4="$TMP/root4"; mkdir -p "$ROOT4"
CONF4="$ROOT4/spira.conf"; TOML4="$ROOT4/spira.toml"
INST4="test$$-c"
printf 'SPIRA_DB = /some/legacy/db\n' > "$CONF4"
conf4_before="$(cat "$CONF4")"

seed "$CONF4" "$TOML4" "$INST4" >/dev/null
if [ -f "$TOML4" ]; then
    ok "legacy-conf: a spira.toml is created at the target root"
else
    bad "legacy-conf: a spira.toml is created at the target root" "missing: $TOML4"
fi
got4="$("$SPIRA_CONFIG_BIN" get spira.instance "$TOML4" 2>/dev/null)"
is "legacy-conf: instance recorded in the new spira.toml" "$INST4" "$got4"
is "legacy-conf: the spira.conf itself is untouched" "$conf4_before" "$(cat "$CONF4")"
nowant "legacy-conf: no SPIRA_INSTANCE line appended to spira.conf" "SPIRA_INSTANCE" "$(cat "$CONF4")"

tl_summary
