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
# 3. AUTO-CONVERT and TOML-WINS-UNCONDITIONALLY — RETIRED (per Ryan 2026-10-05, the
#    one-source-of-config law): conf.sh no longer reads a legacy spira.conf or converts it
#    on the fly when sourced; SPIRA_TOML is the one source, and a bare SPIRA_CONF with no
#    SPIRA_TOML now just refuses (see BOOTSTRAP below). The standalone `spira-config
#    convert` CLI tool — a one-time, explicitly-invoked migration, never a side effect of
#    sourcing conf.sh — is unaffected and still covered below ("convert refuses to replace
#    a larger spira.toml with a smaller one").
#
# defect: sp-zs04v.2, sp-usxfl
# tier: T1
# covers: spira/conf.sh spira-config/*
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-conf-toml.sh"

# ==========================================================================
# spira-config is the tree's own build, found by name on this suite's PATH (sp-gypjk).
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
# A minimal harness tree so conf.sh resolves sensibly; spira-config comes from PATH.
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
        SPIRA_CONF=/nonexistent\
        SPIRA_WATCHERS="$HARNESS/spira/watchers" \
        "$@" \
        bash -c ". '$HARNESS/spira/conf.sh'; printf '%s' \"\${${key}:-}\"" 2>/dev/null
}

# ==========================================================================
echo
echo "T1 — spira.toml -> conf.sh's environment matches spira-config export --sh, key by key:"
# ==========================================================================
FIXTURE="$CRATE/tests/fixtures/golden.toml"
exported="$(spira-config export --sh "$FIXTURE")"
checked=0 mismatch=0
while IFS='=' read -r rawkey rest; do
    [ -n "$rawkey" ] || continue
    want_val="$(eval "printf '%s' $rest")"
    case "$rawkey" in
        COCKPIT_*) key="$rawkey" ;;
        *)         key="SPIRA_$rawkey" ;;
    esac
    got_val="$(conf_val "$key" SPIRA_TOML="$CRATE/tests/fixtures/complete.toml:$FIXTURE")"
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
# AUTO-CONVERT — RETIRED (per Ryan 2026-10-05, the one-source-of-config law). conf.sh's own
# header now says it plainly: "No XDG/HOME/etc search, no legacy spira.conf, no conversion."
# conf.sh sourcing no longer reads a legacy spira.conf and converts it on the fly — SPIRA_TOML
# is the one source, full stop, and a bare SPIRA_CONF with no SPIRA_TOML now just refuses (see
# BOOTSTRAP below). Three sections tested this retired behavior: a legacy spira.conf converted
# and read on first source, that a once-written spira.toml then wins unconditionally over a
# re-edited .conf, and that auto-convert carried the repo-map/every fayth, not just the .conf.
# All three are deleted rather than converted — there is no SPIRA_TOML-based equivalent of
# "convert this legacy file for me on the fly" to redirect them onto. The standalone
# `spira-config convert` CLI tool itself (a one-time, explicitly-invoked migration, never run
# as a side effect of sourcing conf.sh) is unaffected and still covered below.
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
printf 'SPIRA_ID_PREFIX = sp\nSPIRA_PROD = %s\n' "$SHRINK_DIR/prod" > "$SHRINK_CONF"

spira-config convert --conf "$SHRINK_CONF" \
    --repo-map "$CRATE/tests/fixtures/repo-map" \
    --fayth "$CRATE/tests/fixtures/chamber/builder.fayth" \
    --fayth "$CRATE/tests/fixtures/chamber/ops.fayth" \
    --home "$SHRINK_DIR" --out "$SHRINK_TOML" >/dev/null 2>&1
before="$(cat "$SHRINK_TOML" 2>/dev/null)"
want "first (full) conversion has [repo.home]" "[repo.home]" "$before"

if spira-config convert --conf "$SHRINK_CONF" --home "$SHRINK_DIR" \
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
# CONTAINER PERMISSION DENIED (sp-jv49c) — RETIRED along with AUTO-CONVERT above: this case
# was specifically about auto-convert failing with EACCES and silently reverting to a
# derived default instead of spira.conf's real values. With auto-convert gone, a bare
# SPIRA_CONF (no SPIRA_TOML) now refuses outright regardless of SPIRA_REPO's writability —
# there is no "spira.conf's real values survive" case left to construct.
# ==========================================================================
echo
echo "BOOTSTRAP — no spira-config binary anywhere, and no config readable either:"
# ==========================================================================
# sp-ubcgo ("wave 4.5: conf.sh becomes an eval of resolve"): derived-default computation
# itself now lives in spira-config (`resolve --sh-all`), not in bash, so conf.sh can no
# longer finish — not even the PATH tail — without that binary, REGARDLESS of whether a
# config file exists. Before this bead, an install with no spira-config on PATH but also no
# spira.toml/spira.conf to read could still derive every SPIRA_* default in pure bash and
# reach the PATH-tail append below; that bootstrap path is gone on purpose (fail closed,
# never partially-computed — see conf.sh's own "DERIVED DEFAULTS" comment).
BOOT_HOME="$T/boot-home"
mkdir -p "$BOOT_HOME/.cargo/bin"
printf '#!/bin/sh\nexit 0\n' > "$BOOT_HOME/.cargo/bin/cargo"
chmod +x "$BOOT_HOME/.cargo/bin/cargo"
if env -i PATH=/usr/bin:/bin HOME="$BOOT_HOME" SPIRA_CONF=/nonexistent \
    bash -c ". '$HARNESS/spira/conf.sh'" >"$T/boot.out" 2>&1
then
    bad "conf.sh refuses when spira-config is unresolvable, even with no config file" \
        "exited 0 instead of refusing"
else
    ok "conf.sh refuses when spira-config is unresolvable, even with no config file"
fi
want "the refusal names spira-config" "spira-config" "$(cat "$T/boot.out")"

# ==========================================================================
echo
echo "FAIL-CLOSED (sp-c7b85, extended by sp-ubcgo) — a config file exists but spira-config"
echo "is unresolvable:"
# ==========================================================================
# The scar: run by hand without the launcher PATH, conf.sh could not find spira-config
# ("spira-config: command not found", bash's exit 127) and silently fell back to derived
# defaults (SPIRA_DB=~/.local/share/spira/db, ...) instead of refusing — a tool run that
# way could write to a store that is not production's. conf.sh now checks `command -v
# spira-config` itself, before deriving anything, so this refuses identically whether or
# not a config file exists — the BOOTSTRAP case above hits the exact same guard; this one
# just sets SPIRA_TOML explicitly, to show the guard does not depend on there being no
# config to read.
if env -i PATH="/usr/bin:/bin" HOME="$T/home" SPIRA_TOML="$CRATE/tests/fixtures/complete.toml:$FIXTURE" \
    bash -c ". '$HARNESS/spira/conf.sh'" >"$T/unresolvable.out" 2>&1
then
    bad "conf.sh refuses when spira-config is unresolvable and a config file exists" \
        "exited 0 instead of refusing"
else
    ok "conf.sh refuses when spira-config is unresolvable and a config file exists"
fi
want "the refusal names spira-config" "spira-config" "$(cat "$T/unresolvable.out")"
want "the refusal names SPIRA_RELEASE (why spira-config could not be found)" \
    "SPIRA_RELEASE" "$(cat "$T/unresolvable.out")"

if grep -q 'No such file or directory' "$T/unresolvable.out"; then
    bad "the refusal is a named diagnostic, not a bare exec failure" "$(cat "$T/unresolvable.out")"
else
    ok "the refusal is a named diagnostic, not a bare exec failure"
fi

# ==========================================================================
tl_summary
