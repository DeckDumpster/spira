#!/usr/bin/env bash
#
# test-doctor-config-files.sh — doctor.sh warns when both spira.conf and spira.toml exist,
#   and reports which single file (if any) is in force otherwise.
#
# Once spira.toml exists it is the only file conf.sh reads or any writer targets
# (spira_toml_resolve, sp-usxfl); a spira.conf left beside it is inert and easy to miss —
# this is the one place that says so.
#
# CONF-ONLY IS TRANSIENT BY DESIGN. Sourcing conf.sh against a real spira.conf with no
# spira.toml yet auto-converts one into existence as a side effect of the read itself — so
# doctor.sh, which sources conf.sh to run at all, can only ever observe "spira.conf only" when
# that conversion fails (a broken repo-map, here) rather than when it quietly succeeds. That
# is also the only way "conf-only" is reachable in production: the very next tool that reads
# config after a legacy-only box's first boot creates the toml, and from then on both files
# exist until the operator deletes the .conf — which is exactly the state this check exists
# to surface.
#
# POSITIVE CONTROL FIRST (law-absence-needs-a-positive-control): the neither-file OK case
# is checked before the both-files WARN is trusted to fire only when it should.
#
# tier: T1
# covers: spira/doctor.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/testlib.sh"

echo "test-doctor-config-files.sh"

SPIRA_CONFIG_BIN="$(testlib_spira_config_bin)" || skip "no spira-config binary found — cannot be built here"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

mkdir -p "$TMP/bin" "$TMP/db/.beads" "$TMP/run" "$TMP/home" "$TMP/chamber"
: > "$TMP/repo-map"

cat > "$TMP/bin/bd" <<'FAKEBD'
#!/usr/bin/env bash
case "$*" in
    *"migrate schema"*)         printf '✓ Schema already at v61\n'; exit 0 ;;
    *"list"*"--json"*)          printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *"show"*"sp-test"*)         printf '[{"id":"sp-test"}]\n'; exit 0 ;;
    *)                          exit 0 ;;
esac
FAKEBD
chmod +x "$TMP/bin/bd"

cat > "$TMP/bin/fake-notify" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$TMP/bin/fake-notify"

cat > "$TMP/bin/systemctl" <<'SH'
#!/usr/bin/env bash
printf 'enabled\n'; exit 0
SH
chmod +x "$TMP/bin/systemctl"

CONF="$TMP/spira.conf"
TOML="$TMP/spira.toml"

# run_doctor <conf-path> <toml-path> [repo-map] — SPIRA_REPO pinned to $TMP so conf.sh's
# auto-convert (and spira_config_writeback's redirect, should it ever trip) writes inside the
# fixture, never a real checkout (the scar test-conf-writeback.sh guards against). repo-map
# defaults to the empty, valid one built above; a nonexistent path makes the conversion
# itself fail, which is how PROPERTY 3 reaches a genuinely conf-only state below.
run_doctor() {
    env -i \
        PATH="$TMP/bin:/usr/local/bin:/usr/bin:/bin" \
        HOME="$TMP/home" \
        SPIRA_CONF="$1" \
        SPIRA_TOML="$2" \
        SPIRA_REPO="$TMP" \
        SPIRA_REPO_MAP="${3:-$TMP/repo-map}" \
        SPIRA_CHAMBER="$TMP/chamber" \
        SPIRA_CONFIG_BIN="$SPIRA_CONFIG_BIN" \
        SPIRA_PATH="$TMP/bin" \
        SPIRA_DB="$TMP/db" \
        SPIRA_RUN="$TMP/run" \
        SPIRA_BD="$TMP/bin/bd" \
        SPIRA_BD_PIN="$TMP/run/bd-pin" \
        SPIRA_NOTIFY="$TMP/bin/fake-notify" \
        SPIRA_SYSTEMCTL="$TMP/bin/systemctl" \
        SPIRA_GOAL=sp-test \
        SPIRA_COCKPIT="$TMP/run" \
        SPIRA_SNAP_STALE_S=60 \
        bash "$HERE/doctor.sh" 2>/dev/null || true
}

# ==========================================================================
echo
echo "positive control — neither file exists:"
# ==========================================================================
rm -f "$CONF" "$TOML"
none_out="$(run_doctor "$CONF" "$TOML")"
want   "neither: config files section runs"        "config files"                              "$none_out"
want   "neither: OK no config file found"          "ok    no config file found"                 "$none_out"
nowant "neither: no both-files WARN"               "both"                                       "$none_out"

# ==========================================================================
echo
echo "spira.toml only:"
# ==========================================================================
rm -f "$CONF"
printf '[spira]\nmax_aeons = 4\n' > "$TOML"
toml_out="$(run_doctor "$CONF" "$TOML")"
want   "toml-only: OK spira.toml only"             "ok    spira.toml only"                      "$toml_out"
nowant "toml-only: no both-files WARN"             "both"                                       "$toml_out"

# ==========================================================================
echo
echo "spira.conf only (legacy) — conversion fails closed, so the toml never appears:"
# ==========================================================================
rm -f "$TOML"
printf 'SPIRA_MAX_AEONS = 4\n' > "$CONF"
conf_out="$(run_doctor "$CONF" "$TOML" "$TMP/no-such-repo-map")"
if [ ! -e "$TOML" ]; then
    ok "conf-only: conversion did not create a spira.toml (repo-map refused)"
else
    bad "conf-only: conversion did not create a spira.toml" "found: $TOML"
fi
want   "conf-only: OK spira.conf only (legacy)"    "ok    spira.conf only (legacy)"              "$conf_out"
nowant "conf-only: no both-files WARN"             "both"                                        "$conf_out"

# ==========================================================================
echo
echo "both files present — WARN names both paths:"
# ==========================================================================
printf '[spira]\nmax_aeons = 4\n' > "$TOML"
printf 'SPIRA_MAX_AEONS = 4\n' > "$CONF"
both_out="$(run_doctor "$CONF" "$TOML")"
want "both: WARN fires"           "warn  both $CONF and $TOML exist" "$both_out"
want "both: names the fix"        "remove $CONF"                     "$both_out"
nowant "both: no neither-OK line" "no config file found"             "$both_out"

tl_summary
