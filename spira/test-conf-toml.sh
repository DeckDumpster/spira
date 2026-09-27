#!/usr/bin/env bash
#
# test-conf-toml.sh — conf.sh's spira-config cutover (sp-zs04v.2).
#
# WHAT THIS SUITE IS FOR
# -----------------------
# conf.sh now sources `spira-config export --sh` instead of parsing spira.conf itself.
#
# 1. T0: the old KEY=value reader is gone from conf.sh.
# 2. T1: sourcing conf.sh against a fixture spira.toml sets the same environment as
#    `spira-config export --sh` run directly against that file, key for key (the SPIRA_/
#    COCKPIT_ naming remap conf.sh applies is accounted for here, not re-derived).
# 3. AUTO-CONVERT: the production-safety mechanism this bead exists for — a box with only
#    spira.conf (no spira.toml) gets one derived from it, and conf.sh reads the derived
#    file correctly, so retiring the old reader does not silently blank every SPIRA_* key
#    on a box that has never seen spira.toml.
# 4. TOML WINS UNCONDITIONALLY (sp-usxfl): once a spira.toml exists, editing a spira.conf
#    beside it — even to a newer mtime — never regenerates it. Every writer now targets
#    spira.toml directly, so a surviving spira.conf is stale by construction.
#
# defect: sp-zs04v.2, sp-usxfl
# covers: spira/conf.sh spira-config/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-conf-toml.sh"

# ==========================================================================
# Resolve or build the spira-config binary. Skip (not fail) if cargo is unavailable — every
# case below, including T0, needs it. Checked BEFORE any case runs: testlib's skip() bails
# instead of skipping once a case has already recorded (law-a-refusal-names-its-exit).
# ==========================================================================
CARGO_BIN="$(command -v cargo 2>/dev/null || true)"
[ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
if [ -z "$CARGO_BIN" ]; then
    skip "cargo not found — spira-config binary cannot be built"
fi

# ==========================================================================
echo
echo "T0 — the old KEY=value reader is gone from conf.sh:"
# ==========================================================================
if grep -q 'spira_conf_read' "$HERE/conf.sh"; then
    bad "spira_conf_read is gone from conf.sh" "still present in $HERE/conf.sh"
else
    ok "spira_conf_read is gone from conf.sh"
fi

T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT INT TERM
CRATE="$HERE/../spira-config"
SPIRA_CONFIG_BIN="$HERE/../target/release/spira-config"
if [ ! -x "$SPIRA_CONFIG_BIN" ]; then
    printf '  (building spira-config into %s)\n' "$T/target"
    CARGO_TARGET_DIR="$T/target" "$CARGO_BIN" build --release \
        --manifest-path "$CRATE/Cargo.toml" >/dev/null 2>&1
    SPIRA_CONFIG_BIN="$T/target/release/spira-config"
fi
if [ -x "$SPIRA_CONFIG_BIN" ]; then
    ok "spira-config binary is present and executable"
else
    bail "spira-config binary not found/built at $SPIRA_CONFIG_BIN"
fi

# A minimal harness tree so conf.sh resolves sensibly; SPIRA_CONFIG_BIN is passed in
# explicitly since this scratch tree carries no compiled binaries of its own.
HARNESS="$T/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

# conf_val <varname> <extra env...> — the fixed defaults come FIRST so a caller-supplied
# override of the same name (e.g. SPIRA_CONF=...) wins, matching `env`'s last-one-wins rule.
conf_val() {
    local key="$1"; shift
    env -i PATH="$PATH" HOME="$T/home" \
        SPIRA_CONF=/nonexistent SPIRA_CONFIG_BIN="$SPIRA_CONFIG_BIN" \
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        "$@" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${${key}:-}\"" 2>/dev/null
}

# ==========================================================================
echo
echo "T1 — spira.toml -> conf.sh's environment matches spira-config export --sh, key by key:"
# ==========================================================================
FIXTURE="$CRATE/tests/fixtures/golden.toml"
exported="$("$SPIRA_CONFIG_BIN" export --sh "$FIXTURE")"
checked=0 mismatch=0
while IFS='=' read -r rawkey rest; do
    [ -n "$rawkey" ] || continue
    want_val="$(eval "printf '%s' $rest")"
    case "$rawkey" in
        COCKPIT_*) key="$rawkey" ;;
        *)         key="SPIRA_$rawkey" ;;
    esac
    got_val="$(conf_val "$key" SPIRA_TOML="$FIXTURE")"
    checked=$((checked+1))
    if [ "$got_val" != "$want_val" ]; then
        mismatch=$((mismatch+1))
        bad "conf.sh $key matches export --sh" "wanted [$want_val] got [$got_val]"
    fi
done <<< "$exported"
if [ "$checked" -eq 0 ]; then
    bad "T1 fixture produced keys to check" "export --sh printed nothing"
elif [ "$mismatch" -eq 0 ]; then
    ok "every one of $checked exported keys from the fixture matches conf.sh's own environment"
fi

# ==========================================================================
echo
echo "AUTO-CONVERT — a legacy spira.conf with no spira.toml is converted and read:"
# ==========================================================================
LEGACY_DIR="$T/legacy"
mkdir -p "$LEGACY_DIR"
CONF_FILE="$LEGACY_DIR/spira.conf"
conf_prod="$T/legacy-chosen/spira"
printf 'SPIRA_PROD = %s\nSPIRA_MAX_AEONS = 7\n' "$conf_prod" > "$CONF_FILE"

# SPIRA_CONFIG_WRITE=1 bypasses spira_config_writeback's redirect (sp-q5hzx): this
# fixture is not the installed release, so without it every write here would land
# under SPIRA_REPO instead of beside spira.conf. The redirect itself is covered by
# test-conf-writeback.sh; this suite is about the conversion, not the guard.
got_prod="$(conf_val SPIRA_PROD SPIRA_CONF="$CONF_FILE" SPIRA_CONFIG_WRITE=1)"
is "auto-converted SPIRA_PROD is read by conf.sh" "$conf_prod" "$got_prod"
got_max="$(conf_val SPIRA_MAX_AEONS SPIRA_CONF="$CONF_FILE" SPIRA_CONFIG_WRITE=1)"
is "auto-converted SPIRA_MAX_AEONS is read by conf.sh" "7" "$got_max"

if [ -f "$LEGACY_DIR/spira.toml" ]; then
    ok "auto-convert wrote a spira.toml beside spira.conf"
else
    bad "auto-convert wrote a spira.toml beside spira.conf" "not found at $LEGACY_DIR/spira.toml"
fi

# ONCE A spira.toml EXISTS IT WINS UNCONDITIONALLY (sp-usxfl) — no more mtime comparison.
# Editing the .conf again, even to a value newer than the .toml, must NOT regenerate it:
# every writer now targets spira.toml directly (spira_config_set), so a spira.conf that
# still exists is stale by construction, and regenerating from it would silently discard
# whatever the .toml alone has gained since (the "gutted spira.toml" failure this bead
# retires).
sleep 1.1
printf 'SPIRA_PROD = %s/v2\n' "$T" > "$CONF_FILE"
got_prod2="$(conf_val SPIRA_PROD SPIRA_CONF="$CONF_FILE" SPIRA_CONFIG_WRITE=1)"
is "editing spira.conf again does NOT regenerate the existing spira.toml" "$conf_prod" "$got_prod2"

# ==========================================================================
echo
echo "AUTO-CONVERT carries the repo-map and every fayth, not just spira.conf:"
# ==========================================================================
# BLOCKING DEFECT (sp-zs04v.2): the auto-convert call above passed only --conf, so on a
# box that also has a repo-map and chamber/*.fayth (i.e. any real install) it produced a
# spira.toml with an empty [repo] table and no personas, and overwrote the real one with
# it. conf_val's SPIRA_HOME (unset here, so derived from $HARNESS/spira/conf.sh) is where
# _spira_fayth_paths and _spira_repo_map_candidate look.
FULL_DIR="$T/legacy-full"
mkdir -p "$FULL_DIR" "$HARNESS/spira/chamber"
cp "$CRATE/tests/fixtures/repo-map" "$FULL_DIR/repo-map"
cp "$CRATE/tests/fixtures/chamber/builder.fayth" "$CRATE/tests/fixtures/chamber/ops.fayth" \
    "$HARNESS/spira/chamber/"
FULL_CONF="$FULL_DIR/spira.conf"
printf 'SPIRA_PROD = %s\n' "$T/full-chosen/spira" > "$FULL_CONF"

conf_val SPIRA_PROD SPIRA_CONF="$FULL_CONF" SPIRA_CONFIG_WRITE=1 >/dev/null
GENERATED="$FULL_DIR/spira.toml"
generated_text="$(cat "$GENERATED" 2>/dev/null)"
if [ -n "$generated_text" ]; then
    want "auto-convert's spira.toml carries the repo-map's [repo.home] table" \
        "[repo.home]" "$generated_text"
    want "auto-convert's spira.toml carries builder's [persona.builder] table" \
        "[persona.builder]" "$generated_text"
    want "auto-convert's spira.toml carries ops's [persona.ops] table" \
        "[persona.ops]" "$generated_text"
else
    bad "auto-convert with repo-map+fayth present" "no spira.toml produced at $GENERATED"
fi

# ==========================================================================
echo
echo "convert refuses to replace a larger spira.toml with a smaller one:"
# ==========================================================================
# Same defect, isolated to the binary: the exact call the old auto-convert made
# (--conf only, no --repo-map/--fayth) must now be refused rather than accepted, and the
# existing (larger) document must survive untouched.
SHRINK_DIR="$T/shrink"; mkdir -p "$SHRINK_DIR"
SHRINK_TOML="$SHRINK_DIR/spira.toml"
SHRINK_CONF="$SHRINK_DIR/spira.conf"
printf 'SPIRA_PROD = %s\n' "$SHRINK_DIR/prod" > "$SHRINK_CONF"

"$SPIRA_CONFIG_BIN" convert --conf "$SHRINK_CONF" \
    --repo-map "$CRATE/tests/fixtures/repo-map" \
    --fayth "$CRATE/tests/fixtures/chamber/builder.fayth" \
    --fayth "$CRATE/tests/fixtures/chamber/ops.fayth" \
    --home "$SHRINK_DIR" --out "$SHRINK_TOML" >/dev/null 2>&1
before="$(cat "$SHRINK_TOML" 2>/dev/null)"
want "first (full) conversion has [repo.home]" "[repo.home]" "$before"

if "$SPIRA_CONFIG_BIN" convert --conf "$SHRINK_CONF" --home "$SHRINK_DIR" \
    --out "$SHRINK_TOML" >/dev/null 2>&1
then
    bad "convert refuses to shrink the existing document" "exited 0 instead of refusing"
else
    ok "convert refuses to shrink the existing document"
fi
after="$(cat "$SHRINK_TOML" 2>/dev/null)"
is "the existing (larger) spira.toml is left untouched after the refusal" "$before" "$after"
leftover="$(ls "$SHRINK_DIR"/spira.toml.tmp* 2>/dev/null | wc -l | tr -d ' ')"
is "no leftover temp file after the refusal" "0" "$leftover"

# ==========================================================================
echo
echo "CONTAINER PERMISSION DENIED (sp-jv49c) — an unwritable SPIRA_REPO does not cost"
echo "spira.conf's real values:"
# ==========================================================================
# The concrete regression: a testenv container bind-mounts SPIRA_REPO read-write for its
# host owner but read-only (or foreign-UID-owned) for the user conf.sh runs as, and
# spira_toml_resolve's write target used to be $SPIRA_REPO unconditionally. Auto-convert
# then failed with EACCES and, since no spira.toml existed yet either, every SPIRA_* key
# silently reverted to its computed default instead of what spira.conf says. HARNESS
# doubles as SPIRA_REPO here (no .git under it), so chmod on it reproduces the container.
RO_DIR="$T/readonly-legacy"
mkdir -p "$RO_DIR"
RO_CONF="$RO_DIR/spira.conf"
ro_prod="$T/readonly-chosen/spira"
printf 'SPIRA_PROD = %s\n' "$ro_prod" > "$RO_CONF"
chmod a-w "$HARNESS"
got_ro_prod="$(conf_val SPIRA_PROD SPIRA_CONF="$RO_CONF")"
chmod u+w "$HARNESS"
is "SPIRA_PROD survives an unwritable SPIRA_REPO" "$ro_prod" "$got_ro_prod"

# ==========================================================================
echo
echo "BOOTSTRAP — no spira-config binary anywhere: cargo stays reachable:"
# ==========================================================================
# A fresh worktree (no target/, no bin/ — $HARNESS above has neither) cannot resolve
# SPIRA_CONFIG_BIN, so nothing can be read from spira.conf or spira.toml at all: every
# SPIRA_* key falls back to its derived default, including SPIRA_PATH (empty). If cargo
# is reachable only through a configured SPIRA_PATH, this is a deadlock: the binary that
# would let a worktree build its own spira-config needs cargo on PATH, and cargo isn't
# there until spira.toml is read. SPIRA_CONFIG_BIN is deliberately left unset (not passed)
# so the worktree resolves it on its own and finds nothing.
BOOT_HOME="$T/boot-home"
mkdir -p "$BOOT_HOME/.cargo/bin"
printf '#!/bin/sh\nexit 0\n' > "$BOOT_HOME/.cargo/bin/cargo"
chmod +x "$BOOT_HOME/.cargo/bin/cargo"
boot_path="$(env -i PATH=/usr/bin:/bin HOME="$BOOT_HOME" SPIRA_CONF=/nonexistent \
    bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\$PATH\"" 2>/dev/null)"
want "conf.sh's PATH still reaches \$HOME/.cargo/bin with no spira-config binary built" \
    "$BOOT_HOME/.cargo/bin" "$boot_path"

# ==========================================================================
tl_summary
