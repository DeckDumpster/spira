#!/usr/bin/env bash
# land-build-ensure.sh — fire build.sh when cargo source changed or a cargo-built
# binary is absent with its unit enabled.
#
# Called by the landing pass after each sweep; also testable standalone.
#
# Reads from environment (set by conf.sh in the caller):
#   SPIRA_REPO, SPIRA_LOOM_BIN, SPIRA_BROKER_BIN, SPIRA_PANEL,
#   SPIRA_CZAR_PASS_BIN, SPIRA_INSTANCE, SPIRA_SYSTEMCTL.
#
# Exits 0 always; build.sh itself exits 0 on absent cargo (warning only).
#
# covers: spira/landing.sh spira/build.sh
set -uo pipefail

command -v cargo >/dev/null 2>&1 || exit 0

_be_bs="${SPIRA_REPO:-}/spira/build.sh"
[ -x "$_be_bs" ] || exit 0

_be_base_ref="$(git -C "${SPIRA_REPO:-}" symbolic-ref --short HEAD 2>/dev/null || true)"
_be_src_changed=0
if [ -n "$_be_base_ref" ]; then
    _be_prev="$(git -C "${SPIRA_REPO:-}" rev-parse "${_be_base_ref}@{1}" 2>/dev/null || true)"
    if [ -n "$_be_prev" ]; then
        git -C "${SPIRA_REPO:-}" diff --name-only "$_be_prev" HEAD 2>/dev/null \
            | grep -qE '\.rs$' \
            && _be_src_changed=1 || true
    fi
fi

_be_sc="${SPIRA_SYSTEMCTL:-systemctl}"
_be_missing=0
for _be_pair in \
    "${SPIRA_LOOM_BIN:-}:loom:service" \
    "${SPIRA_BROKER_BIN:-}:broker:timer" \
    "${SPIRA_PANEL:-}:cockpit:service" \
    "${SPIRA_CZAR_PASS_BIN:-}:czar-pass:timer"; do
    _be_cbin="${_be_pair%%:*}"
    _be_rest="${_be_pair#*:}"
    _be_cbase="${_be_rest%%:*}"
    _be_ctype="${_be_rest#*:}"
    [ -n "${_be_cbin}" ] || continue
    [ -x "${_be_cbin}" ] && continue
    _be_cinst="spira-${_be_cbase}${SPIRA_INSTANCE:+-$SPIRA_INSTANCE}.${_be_ctype}"
    _be_cplain="spira-${_be_cbase}.${_be_ctype}"
    if "$_be_sc" --user is-enabled "$_be_cinst"  >/dev/null 2>&1 ||
       "$_be_sc" --user is-enabled "$_be_cplain" >/dev/null 2>&1; then
        _be_missing=1; break
    fi
done
unset _be_pair _be_cbin _be_rest _be_cbase _be_ctype _be_cinst _be_cplain

if [ "$_be_src_changed" -eq 1 ]; then
    printf 'landing: cargo source changed — running build.sh\n'
    bash "$_be_bs" 2>&1
elif [ "$_be_missing" -eq 1 ]; then
    printf 'landing: cargo binary absent with unit enabled — running build.sh\n'
    bash "$_be_bs" 2>&1
fi
unset _be_bs _be_base_ref _be_src_changed _be_prev _be_sc _be_missing
