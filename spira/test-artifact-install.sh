#!/usr/bin/env bash
# test-artifact-install.sh — conf.sh resolves SPIRA_LOOM_BIN inside a release, and what
# ends up live after activation is the release just unpacked, not a stale stand-in.
#
# CASE 1. Positive control, then the fix: the OLD default for SPIRA_LOOM_BIN pointed at
#   the source-checkout path (loom/target/release/loom), which a tarball install never
#   has. Prove that path is absent here, then that conf.sh resolves to bin/loom instead.
#
# CASE 2 (Gap G8). release install-tarball (sp-jsnbm; was activate.sh) reuses an
#   already-present, same-named release dir rather than re-unpacking (the mechanism
#   rollback depends on). Plant the PRIOR release with its own marker so the check cannot
#   pass by accident (law-absence-needs-a-positive-control), then activate a fresh release
#   carrying a distinguishable sp-x.fixed marker and confirm that marker — not the prior
#   one — is what current/bin/loom holds.
#
# The cargo build and a live loom process serving /api/beads run once, for real, in
# acceptance.yml (loom-from-release), against the real build-tarball.sh output — not
# hand-assembled here, and not repeated on every branch (docs/test-plan/instance-
# lifecycle.md, UC-instance-lifecycle-15). defect: sp-kcx8.
#
# tier: T1
# covers: spira/conf.sh release/src/install.rs UC-instance-lifecycle-15
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
. "$HERE/testlib.sh"

echo "test-artifact-install.sh"

SCRATCH="$(mktemp -d)"
trap 'chmod -R u+w "$SCRATCH" 2>/dev/null; rm -rf "$SCRATCH"' EXIT INT TERM

# case 1 (conf.sh resolving SPIRA_LOOM_BIN to bin/loom) is deleted with the resolver (sp-gypjk):
# every tool is invoked by name on the launcher's PATH.

# ===========================================================================
echo
echo "case 2 (Gap G8): current/bin/loom is what was just built, not a stale stand-in"
# ===========================================================================
RELEASES="$SCRATCH/releases"
RUN_DIR="$SCRATCH/run"
mkdir -p "$RELEASES" "$RUN_DIR"

STALE_NAME="spira-20200101T000000Z"
mkdir -p "$RELEASES/$STALE_NAME/bin"
printf 'stale-marker\n' > "$RELEASES/$STALE_NAME/bin/loom" && chmod +x "$RELEASES/$STALE_NAME/bin/loom"
ln -s "$STALE_NAME" "$RELEASES/current"
is "positive-control: the stale release's marker is live before activation" \
    "stale-marker" "$(cat "$RELEASES/current/bin/loom")"

FIXED_NAME="spira-20260925T000000Z"
STAGE="$SCRATCH/stage/$FIXED_NAME"
mkdir -p "$STAGE/bin"
printf 'sp-x.fixed\n' > "$STAGE/bin/loom" && chmod +x "$STAGE/bin/loom"
TARBALL="$SCRATCH/$FIXED_NAME.tar.gz"
tar -czf "$TARBALL" -C "$SCRATCH/stage" "$FIXED_NAME"

MOCK_SC="$SCRATCH/mock-systemctl"
printf '#!/bin/sh\nexit 0\n' > "$MOCK_SC" && chmod +x "$MOCK_SC"

# Minimal, isolated environment (mirrors test-activate.sh's replacement, install.rs's own
# unit tests): SPIRA_DB/SPIRA_CONF point at nothing so nothing here can read the operator's
# real store or config. release install-tarball needs no SPIRA_CONF or SPIRA_HOME (its
# roots come from SPIRA_RELEASES/SPIRA_RUN directly), unlike activate.sh's lib.sh sourcing.
tl_config SPIRA_RUN="$RUN_DIR" SPIRA_RELEASES="$RELEASES" SPIRA_INSTANCE=prod
env -i \
    "PATH=$PATH" "HOME=$HOME" \
    "SPIRA_TOML=$SPIRA_TOML" \
    "SPIRA_SYSTEMCTL=$MOCK_SC" \
    release install-tarball "$TARBALL" >&2
wantrc "release install-tarball exits 0" 0 "$?"

want "gap G8: the just-built marker (sp-x.fixed) is what's live after activation" \
    "sp-x.fixed" "$(cat "$RELEASES/current/bin/loom" 2>/dev/null || true)"
nowant "gap G8: the stale prior release's marker is gone from current" \
    "stale-marker" "$(cat "$RELEASES/current/bin/loom" 2>/dev/null || true)"

tl_summary
