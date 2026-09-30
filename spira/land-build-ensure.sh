#!/usr/bin/env bash
# land-build-ensure.sh — fire build.sh when cargo source changed. (A release always carries
# every binary, so the old "binary absent with its unit enabled" trigger is gone — sp-gypjk.)
#
# Called by the landing pass after each sweep; also testable standalone.
#
# Reads from environment (set by conf.sh in the caller):
#   SPIRA_REPO.
#
# Exits 0 always; build.sh itself exits 0 on absent cargo (warning only).
#
# covers: spira/landing.sh spira/build.sh
set -uo pipefail

command -v cargo >/dev/null 2>&1 || exit 0

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

if [ "$_be_src_changed" -eq 1 ]; then
    printf 'landing: cargo source changed — running build.sh\n'
    build.sh 2>&1
fi
unset _be_base_ref _be_src_changed _be_prev
