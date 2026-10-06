#!/usr/bin/env bash
#
# test-conf-writeback.sh — conf.sh never regenerates the operator's spira.toml on a mere
# read, however it got HOME.
#
# THE INCIDENT THIS GUARDS AGAINST (sp-35ru0, wave 4.7): spira_toml_resolve used to
# regenerate [persona.*] from a fresher chamber/*.fayth on every mere SOURCE of conf.sh —
# on the installed release that write landed on the operator's real spira.toml every time
# a release touched a fayth's mtime, i.e. every release.
#
# THE WRITEBACK GUARD AND AUTO-CONVERT THIS SUITE USED TO COVER ARE GONE, not just the
# incident: one source of config (per Ryan 2026-10-05) means spira_toml_file is
# $SPIRA_TOML or nothing — no XDG/HOME search, no legacy spira.conf, no auto-convert, and
# so no write-back target to guess or redirect in the first place. spira_config_writeback
# no longer exists in conf.sh (see conf.sh's spira_toml_file / spira_toml_resolve). The fix
# for the actual incident below is not a safer redirect; it is removing the
# regenerate-on-read entirely.
#
# tier: T1
# covers: spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# A minimal harness tree, same shape test-conf.sh already builds: no .git, so SPIRA_REPO
# derives to the harness root itself rather than any real checkout.
HARNESS="$TMP/harness"
mkdir -p "$HARNESS/spira"
ln -s "$HERE/conf.sh" "$HARNESS/spira/conf.sh"
printf '# empty\n' > "$HARNESS/spira/repo-map.example"
printf '# empty\n' > "$HARNESS/spira/watchers"

# A fixture HOME holding a "full" spira.toml — the shape the operator's real box carries.
FIXHOME="$TMP/home"
mkdir -p "$FIXHOME/.config/spira"
REAL_TOML="$FIXHOME/.config/spira/spira.toml"
cat > "$REAL_TOML" <<'EOF'
[spira]
id_prefix = "sp"
fayths = ["builder", "ops"]

[repo.home]
path = "/srv/checkouts/home"
mode = "push"

[repo.service]
path = "/srv/checkouts/service"
mode = "pr"

[persona.builder]
model = "claude-sonnet-5"
EOF
ORIG_SUM="$(sha256sum "$REAL_TOML" | awk '{print $1}')"

# The write-back guard and its redirect/fallback ladder (SPIRA_CONFIG_WRITE=1 escape hatch,
# SPIRA_HOME==SPIRA_PROD escape hatch, HARNESS redirect, XDG_CONFIG_HOME fallback, the
# double-unwritable scratch fallback, and the "redirect is not a fiction" write-isolation
# check) tested spira_config_writeback, which no longer exists: there is no write-back
# target to guess any more, so there is nothing left to guard or redirect.
# (one source of config, per Ryan 2026-10-05)

# ==========================================================================
echo
echo "spira_toml_resolve NEVER regenerates on a mere read, even with a fresher fayth —"
echo "THE ACTUAL INCIDENT (sp-35ru0, wave 4.7): the write-on-read path is gone, not just"
echo "guarded:"
# ==========================================================================
# Before sp-35ru0, a chamber holding one fayth newer than $REAL_TOML made spira_toml_resolve
# consider $REAL_TOML stale and regenerate it — exactly the sequence that clobbered the
# operator's real file before spira_config_writeback existed, and which kept clobbering it
# afterwards (just into a safe redirect instead) every time a release touched a fayth's
# mtime. Sourcing conf.sh to find out which file is in force must never write anything, so
# this now asserts NO conversion is attempted at all: spira_toml_resolve returns the real
# path unchanged, byte-identical, fayth mtime ignored.
CHAMBER="$TMP/chamber"; mkdir -p "$CHAMBER"
cat > "$CHAMBER/builder.fayth" <<'EOF'
FAYTH_NAME=builder
EOF
touch -d '+1 minute' "$CHAMBER/builder.fayth"

resolved="$(env -i PATH="$PATH" HOME="$FIXHOME" \
    SPIRA_CONF=/nonexistent \
    SPIRA_TOML="$REAL_TOML" \
    SPIRA_WATCHERS="$HARNESS/spira/watchers" \
    SPIRA_CHAMBER="$CHAMBER" \
    bash -c ". '$HARNESS/spira/conf.sh'; spira_toml_resolve" 2>/dev/null)"
is "spira_toml_resolve returns the real spira.toml unchanged — no regenerate-on-read" \
   "$REAL_TOML" "$resolved"
after_sum="$(sha256sum "$REAL_TOML" | awk '{print $1}')"
is "the operator's real spira.toml is byte-identical — a fresher fayth never rewrites it" \
   "$ORIG_SUM" "$after_sum"

tl_summary
