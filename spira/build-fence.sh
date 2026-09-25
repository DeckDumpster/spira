#!/usr/bin/env bash
# build-fence.sh — a compile is a static check of the tree, not a suite.
#
#   build-fence.sh
#
# WHY THIS EXISTS (sp-9uro3). sp-upkae certified green under fences-only certification
# (SPIRA_GATE_SUITES=off) and then broke the whole batch's build job: its new dependency
# resolved a crate the pinned toolchain could not parse, and since there is one workspace
# Cargo.lock, every crate's build failed with it. Fences-only certification runs no suite at
# all, so nothing on that path ever invoked cargo, and the queue had to bisect the batch to
# find the offending branch.
#
# WHAT TRIGGERS A BUILD. Any changed path matching Cargo.toml, Cargo.lock, a crate's src/,
# rust-toolchain(.toml), or the Makefile itself. Anything else — a script, a doc, a bead
# label — is skipped, so a script-only branch pays nothing. The list comes from
# SPIRA_GATE_FILES (gate.sh's pre-computed STATUS<tab>FILE or bare FILE list, the same shape
# select.sh and orphan-test.sh already read) or, failing that, a diff against SPIRA_GATE_BASE.
#
# WITH NEITHER, THIS SKIPS — the same convention orphan-test.sh uses for "no diff context":
# gate.sh always supplies SPIRA_GATE_FILES, so a real certification always has one of the two;
# a call with neither is a direct or scheduled invocation with nothing to judge a diff against,
# not a branch this fence has any evidence about.
#
# It fails CLOSED: `make build` runs under whatever toolchain is already on PATH — the same
# one the gate's own build job uses — and a non-zero exit is a RED certification naming the
# command's own output, not a suite failure.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
cd "$HERE/.." 2>/dev/null || { printf 'build-fence: cannot reach the tree holding %s\n' "$0" >&2; exit 1; }

# touches_build_surface reads STATUS<tab>FILE lines (git diff --name-status, gate.sh's own
# shape) or bare FILE lines from stdin and returns 0 the moment one matches.
touches_build_surface() {
    local line f
    while IFS= read -r line || [ -n "$line" ]; do
        [ -n "$line" ] || continue
        case "$line" in *"	"*) f="${line#*	}" ;; *) f="$line" ;; esac
        case "$f" in
            Cargo.toml|Cargo.lock|Makefile|rust-toolchain|rust-toolchain.toml) return 0 ;;
            */Cargo.toml|*/Cargo.lock|*/rust-toolchain|*/rust-toolchain.toml) return 0 ;;
            */src/*) return 0 ;;
        esac
    done
    return 1
}

changed=""
if [ -f "${SPIRA_GATE_FILES:-}" ]; then
    changed="$(cat "$SPIRA_GATE_FILES")"
elif [ -n "${SPIRA_GATE_BASE:-}" ]; then
    changed="$(git diff --name-status "$SPIRA_GATE_BASE...HEAD" 2>/dev/null)"
else
    printf 'build-fence: SKIP no SPIRA_GATE_FILES or SPIRA_GATE_BASE — no diff to check\n' >&2
    exit 0
fi

if ! printf '%s\n' "$changed" | touches_build_surface; then
    printf 'build-fence: no build-surface file in the diff — skipped\n' >&2
    exit 0
fi

out="$(make build 2>&1)"; rc=$?
if [ "$rc" -ne 0 ]; then
    printf 'build-fence: make build FAILED (rc=%s) — RED certification, not a suite failure\n' "$rc" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'build-fence: make build ok\n' >&2
exit 0
