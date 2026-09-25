#!/usr/bin/env bash
#
# test-migrate-ask.sh — no caller of the retired cockpit/ask.sh remains (UC-operator-channel-13).
#
# The lint-fidelity half that used to live here (hand-copied sender bodies through mail.sh's
# lint) tested copies, not the senders themselves — replaced by the real-emitter test in
# test-mail-real-senders.sh (gap G-05, test-plan-2026-09-23 coverage-map row 05).
#
# LAW-ABSENCE-NEEDS-A-POSITIVE-CONTROL: the grep that finds ask.sh callers must be shown to
# fire before it is shown to be silent — a grep whose pattern never matches passes "no
# callers" trivially.
#
# tier: T0
# covers: spira/lib.sh spira/sentinel.sh spira/watchd.sh spira/skew.sh spira/pilgrimage.sh spira/archivist.sh spira/reflect.sh spira/incident.sh spira/strand.sh spira/mail.sh UC-operator-channel-13
# host-reason: greps the tree; no systemd/gh/network required; no shared state written
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
. "$HERE/testlib.sh"

REPO_ROOT="$(cd "$HERE/.." && pwd)"

echo "grep positive control — the pattern fires before it is trusted to be silent:"
PLANT="$(mktemp)"; trap 'rm -f "$PLANT"' EXIT INT TERM
printf '#!/usr/bin/env bash\nbash cockpit/ask.sh add "some question"\n' > "$PLANT"

grep -E 'cockpit/ask\.sh|SPIRA_ASK[^_]' "$PLANT" >/dev/null 2>&1 \
    && ok  "grep pattern fires on a planted ask.sh caller" \
    || bad "grep pattern fires on a planted ask.sh caller" "pattern never matched"
rm -f "$PLANT"

echo "no ask.sh callers remain in the tree:"
remaining="$(grep -rE 'cockpit/ask\.sh|SPIRA_ASK[^_]' \
    --include='*.sh' --include='*.md' --include='*.conf' \
    --exclude="$(basename "$0")" \
    "$REPO_ROOT/spira" "$REPO_ROOT/cockpit" 2>/dev/null || true)"

[ -z "$remaining" ] \
    && ok  "no ask.sh callers remain in spira/ or cockpit/" \
    || bad "no ask.sh callers remain in spira/ or cockpit/" "$(printf '%s' "$remaining" | head -5)"

[ ! -f "$REPO_ROOT/cockpit/ask.sh" ] \
    && ok  "cockpit/ask.sh is deleted" \
    || bad "cockpit/ask.sh is deleted" "file still exists"

tl_summary
