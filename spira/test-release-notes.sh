#!/usr/bin/env bash
#
# test-release-notes.sh — release-notes.sh on a fixture history: exactly the landed beads between
# two commits, grouped by area, with config/unit/migration sections from the diff, scrubbed.
#
# host-reason: git fixture only; no container or database dependency
# tier: T0
# covers: spira/release-notes.sh .github/workflows/release.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT INT TERM
echo "test-release-notes.sh"
R="$TMP/r"; git init -q "$R"
c() { git -C "$R" -c user.name=t -c user.email=t@t commit -q -m "$1"; }
mkdir -p "$R/queue" "$R/lifecycle/migrations" "$R/spira/conf.d" "$R/systemd" "$R/docs"
echo 1 > "$R/queue/a"; git -C "$R" add -A; c "spira: land sp-old1 — before the range"
PREV="$(git -C "$R" rev-parse HEAD)"
echo 2 > "$R/queue/b"; git -C "$R" add -A; c "spira: land sp-aaa1 — fix the queue stall"
echo x > "$R/lifecycle/migrations/0099-new.sql"; echo x > "$R/lifecycle/m"; git -C "$R" add -A; c "spira: land sp-bbb2 — lifecycle gains a column"
echo x > "$R/spira/conf.d/NEW_KEY"; echo x > "$R/systemd/new.timer"; echo x > "$R/docs/d.md"; git -C "$R" add -A
c "spira: land sp-ccc3.1 — docs read /home/someone/secret on $(hostname -s)"
echo y > "$R/docs/e.md"; git -C "$R" add -A; c "unrelated housekeeping"
THIS="$(git -C "$R" rev-parse HEAD)"
OUT="$("$HERE/release-notes.sh" --repo "$R" --prev "$PREV" --this "$THIS" --tag T2 --prev-tag T1 --run-url http://x/run)"

want "positive control: the in-range bead is listed" "sp-aaa1 — fix the queue stall (fix)" "$OUT"
want "headline counts three" "3 changes since T1" "$OUT"
nowant "bead before the range is absent" "sp-old1" "$OUT"
nowant "non-landing commit is absent" "housekeeping" "$OUT"
want "queue area" $'### Queue and rounds\n\n- sp-aaa1' "$OUT"
want "lifecycle area" $'### Lifecycle\n\n- sp-bbb2' "$OUT"
want "docs area" $'### Docs\n\n- sp-ccc3.1' "$OUT"
want "config key" '- `NEW_KEY`' "$OUT"
want "unit" '- `new.timer`' "$OUT"
want "migration" '0099-new.sql' "$OUT"
want "acceptance link" "http://x/run" "$OUT"
nowant "no /home path" "/home/" "$OUT"
nowant "no host name" "$(hostname -s)" "$OUT"
want "path redacted" "<path>" "$OUT"
tl_summary
