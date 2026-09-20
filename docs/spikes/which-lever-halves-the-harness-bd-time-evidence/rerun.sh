#!/usr/bin/env bash
# rerun.sh — re-take every measurement in this directory.
#
# The spike's proof-of-concept branch is named in ../which-lever-halves-the-harness-bd-time.md;
# check it out somewhere and point POC at its poc/ directory. This file does not spell the
# branch name, because under a gate's env -i the scope label derives from the worktree's own
# directory name and literal-lint then reads a bead id here as a configured label (sp-shhy5).
#
# Needs: bd on PATH; a Go toolchain; a checkout of the bd source at the pinned tag
# (spira/build-bd.sh puts one at $BD_SRC); a shared fixture exported by testdb.sh
# (TESTDB_SHARED / TESTDB_NAME / TESTDB_BASELINE) for the embedded half; and the harness's
# server-mode test database for the sentinel half.
#
# It writes to scratch directories and to the sentinel fixture it builds and drops. It READS
# the production store and never writes to it.
#
set -uo pipefail
POC="${POC:-$(git rev-parse --show-toplevel)/poc}"
GO="${GO:-$HOME/.local/go/bin/go}"
SCRATCH="${SCRATCH:-/var/tmp/bd-lever-rerun}"
mkdir -p "$SCRATCH"

[ -d "$POC" ] || { echo "rerun.sh: $POC not present — check out the spike's poc branch"; exit 1; }

echo "== build the in-process probe =="
( cd "$POC/inproc" && TMPDIR="$SCRATCH" GOTMPDIR="$SCRATCH" CGO_ENABLED=1 \
    "$GO" build -tags gms_pure_go -o "$SCRATCH/probe" . ) || exit 1

echo
echo "== 2: production store, server mode, reads only =="
IDS="$(bd -C "${SPIRA_DB:?set SPIRA_DB}" list --status open --json --limit 10 \
      | python3 -c 'import json,sys; print(",".join(i["id"] for i in json.load(sys.stdin)))')"
"$SCRATCH/probe" -dir "$SPIRA_DB" -n 10 -reads-only -label "${SPIRA_SCOPE_LABEL:-spira}" -ids "$IDS"
"$POC/cli-bench-prod.sh" "$SPIRA_DB" 10 "$IDS"

echo
echo "== 2: embedded fixture, 500 beads =="
FX="$("$POC/fixture.sh" 500 | sed -n 's/^SPIRA_DB=//p')"
cp -rp "$FX" "$SCRATCH/fx-inp"; cp -rp "$FX" "$SCRATCH/fx-cli"
"$SCRATCH/probe" -dir "$SCRATCH/fx-inp" -n 10
BD=bd-embedded "$POC/cli-bench.sh" "$SCRATCH/fx-cli" 10
rm -rf "$SCRATCH/fx-inp" "$SCRATCH/fx-cli" "$FX"

echo
echo "== 3: one real sentinel pass, every bd call by call site =="
SKIPS= KEEP_FIXTURE=1 "$POC/sentinel-callcount.sh" 60

echo
echo "== 4: CHECK 4's workload four ways =="
echo "   (run against the fixture sentinel-callcount.sh printed as KEPT, then drop it)"
