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
# 6. ROUND-TRIP: the generated file is accepted by conf.sh (spira-config validates, conf.sh resolves).
#
# POSITIVE CONTROLS (law-absence-needs-a-positive-control)
#   a. conf.sh refuses an unknown key (proves the round-trip checker would catch a bad key).
#   b. configure.sh must exist before any property above can be asserted.
#
# defect: sp-id7cx
# tier: T1
# covers: spira/configure.sh spira/conf.sh UC-config-store-preflight-07
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"
iszero() { [ "${2:-1}" -eq 0 ] && ok "$1" || bad "$1" "wanted exit 0, got ${2:-?}"; }

echo "test-configure.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM

# conf.sh's spira.conf path calls spira-config by name to auto-convert (sp-zs04v.2, sp-gypjk).

# ==========================================================================
echo
echo "positive control (a) — conf.sh refuses an unknown key:"
# ==========================================================================
# Plant an unknown key in a spira.toml and verify spira-config refuses it. If this fails,
# the round-trip validation below is untestable — it would pass even if configure.sh wrote
# garbage (law-absence-needs-a-positive-control).
# conf.sh no longer reads a legacy spira.conf at all — SPIRA_TOML is the one source of
# config (per Ryan 2026-10-05) — so the positive control now plants the bad key directly in
# a toml file and asks the same `spira-config validate` the round-trip check below uses,
# rather than sourcing conf.sh against a SPIRA_CONF-style file nothing reads any more.
_pc_conf="$TMP/pc-bad.toml"
printf '[spira]\nnonexistent_key_zzzzz = "value"\n' > "$_pc_conf"
_pc_warn="$(spira-config validate "$_pc_conf" 2>&1)"; _pc_rc=$?
if [ "$_pc_rc" -ne 0 ] && printf '%s\n' "$_pc_warn" | grep -qi 'unknown'; then
    ok "spira-config validate refuses an unknown key in the config file"
else
    bad "positive control (a)" \
        "spira-config validate did not refuse nonexistent_key_zzzzz — round-trip test would be vacuous: $_pc_warn"
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
OUT="$FAKE_HOME/.config/spira/spira.toml"
FAKE_PROD="$TMP/fake-prod/spira"

mkdir -p "$FAKE_HOME"

run_configure() {
    env -i \
        PATH="$PATH" \
        HOME="$FAKE_HOME" \
        SPIRA_CONF=/nonexistent \
        SPIRA_TOML="$SPIRA_TOML" \
        CONFIGURE_OUT="$OUT" \
        CONFIGURE_PROD="$FAKE_PROD" \
        CONFIGURE_MAX_AEONS="2" \
        CONFIGURE_MAX_LIVE_AEONS="" \
        CONFIGURE_LOOM_ADDR="127.0.0.1:8788" \
        CONFIGURE_DOLT_DATA="" \
        "$HERE/configure.sh" "$@" 2>&1
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
        grep -i "^[[:space:]]*$1[[:space:]]*=" | head -1 | sed 's/.*=[[:space:]]*//; s/^"//; s/"$//'
}

_prod_val="$(_active_key prod)"
[ -n "$_prod_val" ] && ok "SPIRA_PROD is an active line" \
                    || bad "SPIRA_PROD missing as active line" "in: $OUT"
is "SPIRA_PROD value matches CONFIGURE_PROD" "$FAKE_PROD" "$_prod_val"

_max_aeons_val="$(_active_key max_aeons)"
is "SPIRA_MAX_AEONS value is 2 (what we passed)" "2" "$_max_aeons_val"

# SPIRA_MAX_LIVE_AEONS was set to "" — empty means no ceiling, so it is omitted
_max_live_line="$(grep -v '^[[:space:]]*#' "$OUT" 2>/dev/null | \
    grep -i "max_live_aeons" | head -1 || true)"
[ -z "$_max_live_line" ] && ok "SPIRA_MAX_LIVE_AEONS empty is omitted (no ceiling)" \
                          || bad "SPIRA_MAX_LIVE_AEONS written despite empty" "$_max_live_line"

_loom_val="$(_active_key loom_addr)"
is "SPIRA_LOOM_ADDR value is 127.0.0.1:8788" "127.0.0.1:8788" "$_loom_val"

# SPIRA_DOLT_DATA was set to "" — it should appear active (explicit empty)
_dolt_line="$(grep -v '^[[:space:]]*#' "$OUT" 2>/dev/null | \
    grep -i "dolt_data" | head -1 || true)"
[ -n "$_dolt_line" ] && ok "SPIRA_DOLT_DATA is an active line (explicit empty)" \
                       || bad "SPIRA_DOLT_DATA missing as active line" "in: $OUT"

# ==========================================================================
echo
echo "derivable keys commented — at least one non-trap key appears as a comment:"
# ==========================================================================
_commented="$(grep '^[[:space:]]*#' "$OUT" 2>/dev/null | \
    grep -i "spira.db\b\|spira.run\b\|spira.workspaces\b" | head -1 || true)"
[ -n "$_commented" ] && ok "at least one derivable key (SPIRA_DB/RUN/WORKSPACES) appears as a comment" \
                      || bad "no derivable keys found as comments" "in: $OUT"

# ==========================================================================
echo
echo "round-trip — conf.sh accepts the generated file (spira-config validates, conf.sh resolves):"
# ==========================================================================
# Source conf.sh with the generated file as the config and capture stderr.
# Any "unknown key" warning means configure.sh wrote a key conf.sh doesn't recognise.
_rt_warn="$(spira-config validate "$OUT" 2>&1)"; _rt_rc=$?
if [ "$_rt_rc" -ne 0 ]; then
    bad "round-trip" "spira-config rejected the file: $_rt_warn"
else
    ok "spira-config validate accepts the generated file"
fi

# Resolution through conf.sh reads the values back.
_rt_prod="$(env -i PATH="$PATH" HOME="$FAKE_HOME" SPIRA_TOML="$_TL_CONF_BASE:$OUT" SPIRA_CONF=/nonexistent \
    bash -c ". '$HERE/conf.sh' 2>/dev/null; printf %s \"\$SPIRA_PROD\"")"
is "conf.sh resolves SPIRA_PROD from the generated toml" "$FAKE_PROD" "$_rt_prod"

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

# (Cases 7-8, repo-map seeded/preserved, are gone: configure.sh no longer seeds a repo-map —
# the example it copied was deleted per Ryan 2026-10-05; the operator declares the repos.)

# ==========================================================================
echo
tl_summary
