#!/usr/bin/env bash
#
# test-configure.sh — configure.sh bootstraps ~/.config/spira/ for a fresh clone.
#
#   ./test-configure.sh
#
# PROPERTIES UNDER TEST
# ---------------------
# 1. EXECUTABLE: configure.sh exists and is executable.
# 2. NON-INTERACTIVE: runs to completion with only env vars, no stdin reads.
# 3. TRAP KEYS ACTIVE: SPIRA_PROD, SPIRA_MAX_AEONS, SPIRA_MAX_LIVE_AEONS,
#    SPIRA_LOOM_ADDR, SPIRA_DOLT_DATA appear as active (uncommented) lines.
# 4. DERIVABLE KEYS COMMENTED: at least one non-trap key appears as a comment.
# 5. NO OVERWRITE: an existing config file is not touched; the script reports it.
# 6. ROUND-TRIP: the generated file is accepted by conf.sh (no unknown-key warnings).
# 7. REPO-MAP SEEDED: a repo-map file is written in the config directory.
# 8. REPO-MAP PRESERVED: an existing repo-map is not overwritten.
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   a. conf.sh refuses an unknown key (proves the round-trip checker would catch a bad key).
#   b. configure.sh must exist before any property above can be asserted.
#
# defect: sp-id7cx
# tier: T1
# covers: spira/configure.sh spira/conf.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
iszero() { [ "${2:-1}" -eq 0 ] && ok "$1" || bad "$1" "wanted exit 0, got ${2:-?}"; }

echo "test-configure.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# ==========================================================================
echo
echo "positive control (a) — conf.sh refuses an unknown key:"
# ==========================================================================
# Plant an unknown key and verify conf.sh warns about it. If this fails, the
# round-trip validation below is untestable — it would pass even if configure.sh
# wrote garbage (law-absence-needs-a-positive-control).
_pc_conf="$TMP/pc-bad.conf"
printf 'SPIRA_NONEXISTENT_KEY_ZZZZZ = value\n' > "$_pc_conf"
_pc_warn="$(SPIRA_CONF="$_pc_conf" SPIRA_DB=/tmp/pc-nodb-$$ \
    bash -c ". '$HERE/conf.sh'" 2>&1 1>/dev/null || true)"
if printf '%s\n' "$_pc_warn" | grep -q 'unknown key'; then
    ok "conf.sh warns about an unknown key in the config file"
else
    bad "positive control (a)" \
        "conf.sh did not warn about SPIRA_NONEXISTENT_KEY_ZZZZZ — round-trip test would be vacuous"
fi

# ==========================================================================
echo
echo "positive control (b) — configure.sh must exist and be executable:"
# ==========================================================================
if [ -x "$HERE/configure.sh" ]; then
    ok "configure.sh exists and is executable"
else
    bad "configure.sh not found or not executable" \
        "expected: $HERE/configure.sh"
    printf '  (all tests below will fail until configure.sh exists)\n'
    printf '\n  %d passed, %d failed\n' "$_TL_PASS" "$_TL_FAIL"
    exit 1
fi

# ==========================================================================
echo
echo "non-interactive run — all trap keys supplied via env vars:"
# ==========================================================================
# A controlled environment: a fake HOME with no pre-existing config, and all
# CONFIGURE_* vars set so no prompt is ever needed. SPIRA_CONF=/nonexistent
# keeps conf.sh from reading any real config on this box during the run.
FAKE_HOME="$TMP/home"
OUT="$FAKE_HOME/.config/spira/spira.conf"
FAKE_PROD="$TMP/fake-prod/spira"

mkdir -p "$FAKE_HOME"

run_configure() {
    env -i \
        PATH="$PATH" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        CONFIGURE_OUT="$OUT" \
        CONFIGURE_PROD="$FAKE_PROD" \
        CONFIGURE_MAX_AEONS="2" \
        CONFIGURE_MAX_LIVE_AEONS="" \
        CONFIGURE_LOOM_ADDR="127.0.0.1:8788" \
        CONFIGURE_DOLT_DATA="" \
        bash "$HERE/configure.sh" --no-repo-map "$@" 2>&1
}

_out="$(run_configure)"; _rc=$?
iszero "configure.sh exits 0" "$_rc"
[ -f "$OUT" ] && ok "config file was created at OUT" \
               || bad "config file not created" "expected $OUT"

# ==========================================================================
echo
echo "trap keys active — each appears as an uncommented KEY = value line:"
# ==========================================================================
# Read the generated config and check for active (non-comment) lines for trap keys.
_content="$(grep -v '^[[:space:]]*#' "$OUT" 2>/dev/null | grep -v '^[[:space:]]*$' || true)"

_active_key() {
    # Returns the value of KEY if it appears as an active line in the generated config
    grep -v '^[[:space:]]*#' "$OUT" 2>/dev/null | \
        grep -i "^[[:space:]]*$1[[:space:]]*=" | head -1 | sed 's/.*=[[:space:]]*//'
}

_prod_val="$(_active_key SPIRA_PROD)"
[ -n "$_prod_val" ] && ok "SPIRA_PROD is an active line" \
                    || bad "SPIRA_PROD missing as active line" "in: $OUT"
is "SPIRA_PROD value matches CONFIGURE_PROD" "$FAKE_PROD" "$_prod_val"

_max_aeons_val="$(_active_key SPIRA_MAX_AEONS)"
is "SPIRA_MAX_AEONS value is 2 (what we passed)" "2" "$_max_aeons_val"

# SPIRA_MAX_LIVE_AEONS was set to "" — it should appear active (explicit empty)
_max_live_line="$(grep -v '^[[:space:]]*#' "$OUT" 2>/dev/null | \
    grep -i "SPIRA_MAX_LIVE_AEONS" | head -1 || true)"
[ -n "$_max_live_line" ] && ok "SPIRA_MAX_LIVE_AEONS is an active line (explicit empty)" \
                          || bad "SPIRA_MAX_LIVE_AEONS missing as active line" "in: $OUT"

_loom_val="$(_active_key SPIRA_LOOM_ADDR)"
is "SPIRA_LOOM_ADDR value is 127.0.0.1:8788" "127.0.0.1:8788" "$_loom_val"

# SPIRA_DOLT_DATA was set to "" — it should appear active (explicit empty)
_dolt_line="$(grep -v '^[[:space:]]*#' "$OUT" 2>/dev/null | \
    grep -i "SPIRA_DOLT_DATA" | head -1 || true)"
[ -n "$_dolt_line" ] && ok "SPIRA_DOLT_DATA is an active line (explicit empty)" \
                       || bad "SPIRA_DOLT_DATA missing as active line" "in: $OUT"

# ==========================================================================
echo
echo "derivable keys commented — at least one non-trap key appears as a comment:"
# ==========================================================================
_commented="$(grep '^[[:space:]]*#' "$OUT" 2>/dev/null | \
    grep -i "SPIRA_DB\b\|SPIRA_RUN\b\|SPIRA_WORKSPACES\b" | head -1 || true)"
[ -n "$_commented" ] && ok "at least one derivable key (SPIRA_DB/RUN/WORKSPACES) appears as a comment" \
                      || bad "no derivable keys found as comments" "in: $OUT"

# ==========================================================================
echo
echo "round-trip — conf.sh accepts the generated file (no unknown-key warnings):"
# ==========================================================================
# Source conf.sh with the generated file as the config and capture stderr.
# Any "unknown key" warning means configure.sh wrote a key conf.sh doesn't recognise.
_rt_warn="$(SPIRA_CONF="$OUT" SPIRA_DB="/tmp/configure-test-nodb-$$" \
    bash -c ". '$HERE/conf.sh'" 2>&1 1>/dev/null || true)"
if printf '%s\n' "$_rt_warn" | grep -q 'unknown key'; then
    bad "round-trip" "conf.sh rejected a key: $_rt_warn"
else
    ok "conf.sh accepted every key in the generated file"
fi

# ==========================================================================
echo
echo "no overwrite — an existing config is reported, not overwritten:"
# ==========================================================================
EXISTING_MARKER="# existing-file-sentinel-$$"
printf '%s\n' "$EXISTING_MARKER" >> "$OUT"

_overwrite_out="$(run_configure 2>&1)"; _overwrite_rc=$?
# It should NOT exit 0 (it must refuse to overwrite)
if [ "$_overwrite_rc" -ne 0 ]; then
    ok "configure.sh exits non-zero when config exists"
else
    bad "no-overwrite" "configure.sh exited 0 when the file already existed"
fi
# The file must still contain the marker (not be replaced)
if grep -qF "$EXISTING_MARKER" "$OUT" 2>/dev/null; then
    ok "existing config file was not overwritten"
else
    bad "no-overwrite" "sentinel line was lost — file was overwritten"
fi
# The output must mention the existing file
want "output reports the existing file" "already exists" "$_overwrite_out"

# ==========================================================================
echo
echo "repo-map seeded — configure.sh seeds a repo-map from the example:"
# ==========================================================================
FAKE_HOME2="$TMP/home2"
OUT2="$FAKE_HOME2/.config/spira/spira.conf"
mkdir -p "$FAKE_HOME2"

run_configure2() {
    env -i \
        PATH="$PATH" \
        HOME="$FAKE_HOME2" \
        SPIRA_CONF=/nonexistent \
        CONFIGURE_OUT="$OUT2" \
        CONFIGURE_PROD="$FAKE_PROD" \
        CONFIGURE_MAX_AEONS="2" \
        CONFIGURE_MAX_LIVE_AEONS="" \
        CONFIGURE_LOOM_ADDR="127.0.0.1:8788" \
        CONFIGURE_DOLT_DATA="" \
        bash "$HERE/configure.sh" "$@" 2>&1
}

_seed_out="$(run_configure2)"; _seed_rc=$?
iszero "configure.sh (with repo-map) exits 0" "$_seed_rc"
_repo_map_path="$(dirname "$OUT2")/repo-map"
[ -f "$_repo_map_path" ] && ok "repo-map was created" \
                          || bad "repo-map not created" "expected $OUT2/../repo-map"

# ==========================================================================
echo
echo "repo-map preserved — existing repo-map is not overwritten:"
# ==========================================================================
EXISTING_MAP_MARKER="# existing-repo-map-sentinel-$$"
printf '%s\n' "$EXISTING_MAP_MARKER" >> "$_repo_map_path"

FAKE_HOME3="$TMP/home3"
OUT3="$FAKE_HOME3/.config/spira/spira.conf"
mkdir -p "$FAKE_HOME3"/.config/spira
cp "$_repo_map_path" "$FAKE_HOME3/.config/spira/repo-map"

_preserve_out="$(env -i \
    PATH="$PATH" \
    HOME="$FAKE_HOME3" \
    SPIRA_CONF=/nonexistent \
    CONFIGURE_OUT="$OUT3" \
    CONFIGURE_PROD="$FAKE_PROD" \
    CONFIGURE_MAX_AEONS="2" \
    CONFIGURE_MAX_LIVE_AEONS="" \
    CONFIGURE_LOOM_ADDR="127.0.0.1:8788" \
    CONFIGURE_DOLT_DATA="" \
    bash "$HERE/configure.sh" 2>&1)"

if grep -qF "$EXISTING_MAP_MARKER" "$FAKE_HOME3/.config/spira/repo-map" 2>/dev/null; then
    ok "existing repo-map was not overwritten"
else
    bad "repo-map preserved" "sentinel line was lost — existing repo-map was overwritten"
fi
want "output reports the existing repo-map" "already exists" "$_preserve_out"

# ==========================================================================
echo
tl_summary
