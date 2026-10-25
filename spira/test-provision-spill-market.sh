#!/usr/bin/env bash
#
# test-provision-spill-market.sh — every use of the ephemeral-ci provision action passes the
# org-wide spot switch (spill-market: ${{ vars.SPILL_MARKET }}). A composite action cannot
# read vars itself, so a caller that omits the input silently opts out of the switch.
# A call may pin a market instead, with a comment on the line above the input saying why.
#
# POSITIVE CONTROL: a planted call without the input must be reported before silence on the
# real workflows is believed.
#
# host-reason: parses checked-in YAML with python; no container or database dependency
#
# tier: T0
# covers: .github/workflows/*.yml
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="$(cd "$HERE/.." && pwd -P)"
. "$HERE/testlib.sh"

checker='
import re, sys
rc = 0
for p in sys.argv[1:]:
    lines = open(p).read().splitlines()
    for i, l in enumerate(lines):
        if not re.search(r"uses:\s*\S*ephemeral-ci/provision@", l):
            continue
        indent = len(l) - len(l.lstrip())
        j = i + 1
        block = []
        while j < len(lines) and (not lines[j].strip() or len(lines[j]) - len(lines[j].lstrip()) > indent
                                  or (lines[j].lstrip().startswith("#"))):
            block.append(j); j += 1
        ok = False
        for k in block:
            m = re.match(r"\s*spill-market:\s*(.*)$", lines[k])
            if not m:
                continue
            if "vars.SPILL_MARKET" in m.group(1):
                ok = True
            elif k > 0 and lines[k - 1].lstrip().startswith("#"):
                ok = True
        if not ok:
            print("%s:%d: provision call without spill-market: ${{ vars.SPILL_MARKET }} (or a commented pin)" % (p, i + 1))
            rc = 1
sys.exit(rc)
'

plant="$(mktemp)"; trap 'rm -f "$plant"' EXIT
printf 'jobs:\n  p:\n    steps:\n      - uses: X/ephemeral-ci/provision@v2\n        with:\n          spill: ec2-only\n' >"$plant"
python3 -I -c "$checker" "$plant" >/dev/null 2>&1
[ $? != 0 ] && ok "positive control detects a call without spill-market" || { bad "positive control" "missing spill-market not detected"; exit 1; }

printf 'jobs:\n  p:\n    steps:\n      - uses: X/ephemeral-ci/provision@v2\n        with:\n          # pinned: reason\n          spill-market: spot\n' >"$plant"
python3 -I -c "$checker" "$plant" >/dev/null 2>&1
wantrc "a commented pin is accepted" 0 $?

out="$(python3 -I -c "$checker" "$ROOT"/.github/workflows/*.yml 2>&1)"
wantrc "every provision call carries spill-market: $out" 0 $?
